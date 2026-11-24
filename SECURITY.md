# Security policy

## Reporting a vulnerability

Please do not open a public issue for a security problem. Use GitHub's private reporting instead: on the repository page, open the **Security** tab and choose **Report a vulnerability**. Include the version, how to reproduce the problem and what an attacker could do with it.

We will acknowledge the report, keep you informed while we work on a fix, and credit you in the release notes unless you prefer otherwise.

## Supported versions

Trailhound is pre-1.0. Fixes are made on the latest release only.

## What to know before exposing a server

- **No TLS.** The server speaks plain HTTP. Put a TLS-terminating proxy in front of it before exposing it to a network you do not control.
- **Keys are stored in clear text** in the data folder (`platform.json` and each tenant's folder). Protect that folder like a secrets file.
- **The admin key gives full control** of every tenant. Do not expose `/_admin` to the internet; restrict it at the proxy.
- **A tenant's master key gives full control of that tenant.** Use the search key, or a key limited to the `search` action, in browsers and mobile apps.
- **There is no per-tenant rate limiting.** Tenant quotas limit indexes, documents and disk, not request rate.
