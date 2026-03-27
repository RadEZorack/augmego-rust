Place local TLS certs for `dev.augmego.ca` in this directory:

- `dev.augmego.ca.pem`
- `dev.augmego.ca-key.pem`

One simple option is `mkcert`:

```bash
mkcert -install
mkcert -cert-file dev-proxy/certs/dev.augmego.ca.pem -key-file dev-proxy/certs/dev.augmego.ca-key.pem dev.augmego.ca
```

You also need a local hosts entry pointing the domain to your machine:

```text
127.0.0.1 dev.augmego.ca
```
