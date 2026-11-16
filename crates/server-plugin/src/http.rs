//! A small JSON client for the control plane over `wasi:http`.
//!
//! Pumpkin runs one call into this plugin at a time, and the plugin's event
//! handlers are called from inside the tick. So a call that waited on the
//! network would hold up the tick whenever a player moved meanwhile. Requests
//! are started with [`Client::start_post`] and checked with [`Pending::poll`]
//! on later runs of the task instead; nothing waits on the network except
//! [`Client::post`] and [`Client::delete`] on unload, after the players are
//! gone. Each request also has short timeouts.

use std::time::{Duration, Instant};

use hallow_proto::ApiError;
use serde::Serialize;
use wasip2::http::{outgoing_handler, types};
use wasip2::io::streams::StreamError;

/// Connect, first-byte and between-bytes timeouts, in milliseconds.
const TIMEOUT_MS: u64 = 2000;
/// Give up on a request that hasn't answered by then, whatever the host's
/// timeouts did.
const GIVE_UP: Duration = Duration::from_secs(10);

#[derive(Debug)]
pub enum Error {
    /// The request didn't complete: refused, timed out, or cut off.
    Transport(String),
    /// The control plane answered with an error status.
    Status(u16, Option<ApiError>),
    /// The body didn't parse.
    Body(String),
}

impl Error {
    /// The server isn't registered: register again.
    pub fn is_not_found(&self) -> bool {
        matches!(self, Self::Status(404, _))
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(err) => write!(f, "can't reach the control plane: {err}"),
            Self::Status(status, Some(err)) => {
                write!(f, "{status} {:?}: {}", err.code, err.message)
            }
            Self::Status(status, None) => write!(f, "status {status}"),
            Self::Body(err) => write!(f, "bad response: {err}"),
        }
    }
}

#[derive(Clone)]
pub struct Client {
    /// `host:port` of the control plane.
    pub authority: String,
    pub token: String,
}

enum Method {
    Get,
    Post,
    Delete,
}

/// A request on its way; check it with [`Pending::poll`].
pub struct Pending {
    response: types::FutureIncomingResponse,
    started: Instant,
}

impl Pending {
    /// `None` while there's no answer yet, then the result once.
    pub fn poll(&self) -> Option<Result<(), Error>> {
        self.answer()
            .map(|answer| answer.and_then(|a| finish(a, false)))
    }

    /// Like [`Pending::poll`], with a `2xx` answer's body. The body is read
    /// once the answer's head is in, waiting for the rest of it: the control
    /// plane sends small bodies along with the head, so this doesn't wait on
    /// the network in practice.
    pub fn poll_body(&self) -> Option<Result<Vec<u8>, Error>> {
        self.answer()
            .map(|answer| answer.and_then(finish_with_body))
    }

    fn answer(&self) -> Option<Result<Answer, Error>> {
        // The host only notices the answer when the future's pollable is
        // checked; `get` alone returns `None` forever (wasmtime-wasi-http).
        // `ready` checks once without waiting.
        if !self.response.subscribe().ready() {
            return (self.started.elapsed() >= GIVE_UP)
                .then(|| Err(Error::Transport("no answer in time".into())));
        }
        match self.response.get() {
            None if self.started.elapsed() >= GIVE_UP => {
                Some(Err(Error::Transport("no answer in time".into())))
            }
            None => None,
            Some(answer) => Some(Ok(answer)),
        }
    }
}

impl Client {
    /// Starts a POST answered with an empty `2xx`, without waiting for it.
    pub fn start_post<B: Serialize>(&self, path: &str, body: &B) -> Result<Pending, Error> {
        let json = serde_json::to_vec(body).map_err(|err| Error::Body(err.to_string()))?;
        Ok(Pending {
            response: self.start(Method::Post, path, Some(&json))?,
            started: Instant::now(),
        })
    }

    /// Starts a GET, without waiting for it; read the answer with
    /// [`Pending::poll_body`].
    pub fn start_get(&self, path: &str) -> Result<Pending, Error> {
        Ok(Pending {
            response: self.start(Method::Get, path, None)?,
            started: Instant::now(),
        })
    }

    /// Sends a POST and waits for the answer; only for unload.
    pub fn post<B: Serialize>(&self, path: &str, body: &B) -> Result<(), Error> {
        let json = serde_json::to_vec(body).map_err(|err| Error::Body(err.to_string()))?;
        Self::wait(self.start(Method::Post, path, Some(&json))?)
    }

    /// Sends a DELETE and waits for the answer.
    pub fn delete(&self, path: &str) -> Result<(), Error> {
        Self::wait(self.start(Method::Delete, path, None)?)
    }

    fn wait(response: types::FutureIncomingResponse) -> Result<(), Error> {
        response.subscribe().block();
        let result = response
            .get()
            .ok_or_else(|| Error::Transport("no response".into()))?;
        finish(result, true)
    }

    /// Sends a request; the response arrives on the returned future.
    fn start(
        &self,
        method: Method,
        path: &str,
        body: Option<&[u8]>,
    ) -> Result<types::FutureIncomingResponse, Error> {
        let mut headers = vec![(
            "authorization".to_owned(),
            format!("Bearer {}", self.token).into_bytes(),
        )];
        if body.is_some() {
            headers.push(("content-type".to_owned(), b"application/json".to_vec()));
        }
        let headers = types::Fields::from_list(&headers).map_err(|err| transport(&err))?;
        let request = types::OutgoingRequest::new(headers);
        let method = match method {
            Method::Get => types::Method::Get,
            Method::Post => types::Method::Post,
            Method::Delete => types::Method::Delete,
        };
        let _ = request.set_method(&method);
        let _ = request.set_scheme(Some(&types::Scheme::Http));
        let _ = request.set_authority(Some(&self.authority));
        let _ = request.set_path_with_query(Some(path));

        let outgoing = request.body().map_err(|()| transport(&"no request body"))?;
        if let Some(bytes) = body {
            let stream = outgoing
                .write()
                .map_err(|()| transport(&"no body stream"))?;
            // Written without waiting: the host buffers the whole body (a
            // write may be up to `check-write` bytes, a megabyte by default),
            // and nothing reads it until the request is sent below.
            let mut rest = bytes;
            while !rest.is_empty() {
                let room = stream.check_write().map_err(|err| transport(&err))?;
                let take = usize::try_from(room).unwrap_or(usize::MAX).min(rest.len());
                if take == 0 {
                    return Err(transport(&"request body too large to buffer"));
                }
                stream.write(&rest[..take]).map_err(|err| transport(&err))?;
                rest = &rest[take..];
            }
            stream.flush().map_err(|err| transport(&err))?;
        }

        let options = types::RequestOptions::new();
        let timeout_ns = TIMEOUT_MS * 1_000_000;
        let _ = options.set_connect_timeout(Some(timeout_ns));
        let _ = options.set_first_byte_timeout(Some(timeout_ns));
        let _ = options.set_between_bytes_timeout(Some(timeout_ns));

        let response =
            outgoing_handler::handle(request, Some(options)).map_err(|err| transport(&err))?;
        types::OutgoingBody::finish(outgoing, None).map_err(|err| transport(&err))?;
        Ok(response)
    }
}

fn transport(err: &dyn std::fmt::Debug) -> Error {
    Error::Transport(format!("{err:?}"))
}

type Answer = Result<Result<types::IncomingResponse, types::ErrorCode>, ()>;

/// `Ok` for a `2xx` answer, whose body is ignored. An error's body is read
/// for its message: without `wait`, only as much as has arrived.
fn finish(answer: Answer, wait: bool) -> Result<(), Error> {
    let response = answer
        .map_err(|()| transport(&"response already taken"))?
        .map_err(|err| transport(&err))?;
    let status = response.status();
    if (200..300).contains(&status) {
        return Ok(());
    }
    let message = read_body(&response, wait)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
    Err(Error::Status(status, message))
}

/// A `2xx` answer's whole body, or the error.
fn finish_with_body(answer: Answer) -> Result<Vec<u8>, Error> {
    let response = answer
        .map_err(|()| transport(&"response already taken"))?
        .map_err(|err| transport(&err))?;
    let status = response.status();
    let body = read_body(&response, true).map_err(Error::Transport);
    if (200..300).contains(&status) {
        return body;
    }
    let message = body
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
    Err(Error::Status(status, message))
}

/// Reads the body; without `wait`, only what has already arrived.
fn read_body(response: &types::IncomingResponse, wait: bool) -> Result<Vec<u8>, String> {
    let body = response
        .consume()
        .map_err(|()| "body already taken".to_owned())?;
    let mut bytes = Vec::new();
    {
        let stream = body.stream().map_err(|()| "no body stream".to_owned())?;
        loop {
            let read = if wait {
                stream.blocking_read(64 * 1024)
            } else {
                stream.read(64 * 1024)
            };
            match read {
                Ok(chunk) if chunk.is_empty() && !wait => break,
                Ok(chunk) => bytes.extend_from_slice(&chunk),
                Err(StreamError::Closed) => break,
                Err(err) => return Err(format!("{err:?}")),
            }
        }
    }
    drop(types::IncomingBody::finish(body));
    Ok(bytes)
}
