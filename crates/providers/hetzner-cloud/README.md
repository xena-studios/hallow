# hallow-provider-hetzner-cloud

Hetzner Cloud for Hallow's fleet manager: hourly burst nodes through the [Hetzner Cloud API](https://docs.hetzner.cloud/), behind [`hallow-fleet`](../../fleet/README.md)'s `Provider` trait.

```rust
use hallow_provider_hetzner_cloud::{ApiToken, Config, HetznerCloud};

// The token comes from the secrets store, never from config.
let hetzner = HetznerCloud::new(ApiToken::new(token), Config::default())?;
// Dry run is the default: wrap it until the provider is trusted.
let provider = hallow_fleet::DryRun::new(hetzner, cached_prices);
```

| Trait method | API call |
|---|---|
| `create` | `POST /v1/servers` with `name`, `server_type`, `image`, `location`, `user_data`, `labels`, `start_after_create` |
| `status` | `GET /v1/servers/{id}` |
| `cancel` | `GET /v1/servers/{id}`, then `DELETE /v1/servers/{id}` |
| `list` | `GET /v1/servers?label_selector=hallow=node`, every page |
| `prices` | `GET /v1/pricing` |

- **Only Hallow's machines.** Every server it creates is labelled `hallow=node` (and `hallow-kind=burst`). `list` asks only for that label and checks it again. `status` and `cancel` refuse a server without it (`NotManaged`), so other servers in the same Hetzner project are safe. Cancelling a server that is already gone succeeds.
- **Costs** come from the server type's price for the server's location: hourly, capped at the monthly price. `Config::price_basis` picks net (default) or gross.
- **Errors** map to `ProviderError`:
  - 429 or `rate_limit_exceeded` becomes `RateLimited`, with the wait taken from `Retry-After` or `RateLimit-Reset`;
  - 401 becomes `Unauthorized` and 403 `Forbidden`;
  - 404 becomes `NotFound`;
  - `resource_limit_exceeded` becomes `QuotaExceeded`;
  - 409 or `uniqueness_error` becomes `Conflict`;
  - 400 or 422 becomes `InvalidRequest`;
  - 5xx becomes `Unavailable`.

  Timeouts (`Config::timeout`, default 30 s; connect 10 s) become `Timeout`. `ProviderError::is_retryable` says which are worth retrying.
- **HTTP**: reqwest with rustls. `Config::base_url` defaults to `https://api.hetzner.cloud/v1`.

The API token needs read and write access to the project. Hetzner tokens can't be scoped more finely than that, which is why the label checks matter. Use a project that holds only Hallow's machines.

## Tests

`tests/mock_api.rs` runs the provider against a mock Hetzner API (axum, in process) that checks the bearer token and records every request. The tests cover:
- the create body and labels;
- label filtering and pagination;
- status and cancel, including refusing servers without Hallow's label;
- prices, net and gross;
- error mapping: bad token, invalid type, duplicate name, rate limit with its reset time, and timeout.

They never call the real API.

```sh
cargo test -p hallow-provider-hetzner-cloud
```
