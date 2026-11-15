//! A small JSON client for the control plane over `wasi:http`, the same as
//! the Vine plugin's.
//!
//! A call waits for its response, and Pumpkin won't run anything else in this
//! plugin until it returns. It runs on a Tokio task, not on the tick (see the
//! crate docs), but each call still has short timeouts so a slow control
//! plane can't hold the plugin.

use hallow_proto::ApiError;
use serde::Serialize;
use wasip2::http::{outgoing_handler, types};
use wasip2::io::streams::StreamError;

/// Connect, first-byte and between-bytes timeouts, in milliseconds.
const TIMEOUT_MS: u64 = 2000;
/// `blocking-write-and-flush` takes at most 4096 bytes at a time.
const WRITE_CHUNK: usize = 4096;

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
    Post,
    Delete,
}

impl Client {
    /// A POST answered with an empty `2xx`.
    pub fn post_empty<B: Serialize>(&self, path: &str, body: &B) -> Result<(), Error> {
        let json = serde_json::to_vec(body).map_err(|err| Error::Body(err.to_string()))?;
        self.send(Method::Post, path, Some(&json)).map(drop)
    }

    pub fn delete(&self, path: &str) -> Result<(), Error> {
        self.send(Method::Delete, path, None).map(drop)
    }

    /// Sends a request and returns the body of a `2xx` response.
    fn send(&self, method: Method, path: &str, body: Option<&[u8]>) -> Result<Vec<u8>, Error> {
        let transport = |err: &dyn std::fmt::Debug| Error::Transport(format!("{err:?}"));

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
            for chunk in bytes.chunks(WRITE_CHUNK) {
                stream
                    .blocking_write_and_flush(chunk)
                    .map_err(|err| transport(&err))?;
            }
        }

        let options = types::RequestOptions::new();
        let timeout_ns = TIMEOUT_MS * 1_000_000;
        let _ = options.set_connect_timeout(Some(timeout_ns));
        let _ = options.set_first_byte_timeout(Some(timeout_ns));
        let _ = options.set_between_bytes_timeout(Some(timeout_ns));

        let future =
            outgoing_handler::handle(request, Some(options)).map_err(|err| transport(&err))?;
        types::OutgoingBody::finish(outgoing, None).map_err(|err| transport(&err))?;
        future.subscribe().block();
        let response = future
            .get()
            .ok_or_else(|| transport(&"no response"))?
            .map_err(|()| transport(&"response already taken"))?
            .map_err(|err| transport(&err))?;

        let status = response.status();
        let bytes = read_body(&response).map_err(|err| transport(&err))?;
        if (200..300).contains(&status) {
            Ok(bytes)
        } else {
            Err(Error::Status(status, serde_json::from_slice(&bytes).ok()))
        }
    }
}

fn read_body(response: &types::IncomingResponse) -> Result<Vec<u8>, String> {
    let body = response
        .consume()
        .map_err(|()| "body already taken".to_owned())?;
    let mut bytes = Vec::new();
    {
        let stream = body.stream().map_err(|()| "no body stream".to_owned())?;
        loop {
            match stream.blocking_read(64 * 1024) {
                Ok(chunk) => bytes.extend_from_slice(&chunk),
                Err(StreamError::Closed) => break,
                Err(err) => return Err(format!("{err:?}")),
            }
        }
    }
    drop(types::IncomingBody::finish(body));
    Ok(bytes)
}
