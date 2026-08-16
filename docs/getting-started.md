# Install Signal Fish Server

For the fastest Docker or Cargo setup, follow the
[Quick Start](quickstart.md#1-start-the-server). It includes the local
configuration needed for a fresh source checkout.

Other ways to run the server:

> **Trusted development networks only:** Both options below use
> `config.example.json`, which binds on all network interfaces and leaves
> browser origins, app IDs, and metrics open. Keep them behind a firewall on a
> trusted network, or use the loopback-bound Docker command in the
> [Quick Start](quickstart.md#1-start-the-server).

- **Docker Compose:** clone the repository and run `docker compose up`.
  Compose mounts the included `config.example.json` for local development.
- **Prebuilt binary:** download your platform's archive from
  [GitHub Releases](https://github.com/Ambiguous-Interactive/signal-fish-server/releases),
  then extract it and change into the extracted directory. Release archives do
  not contain a config file, so download the example that matches the release
  as `config.json` before launching the executable from that directory:

  ```bash
  curl --fail --location --output config.json \
    https://raw.githubusercontent.com/Ambiguous-Interactive/signal-fish-server/v0.7.0/config.example.json
  ./signal-fish-server
  ```

  Replace `v0.6.0` with the tag of the release you downloaded. On Windows, run
  `.\signal-fish-server.exe` instead. The server checks its working directory
  for `config.json`, so keep the launch command in the extracted directory.

Continue with [creating and joining a room](quickstart.md#2-create-a-room), or
read [Configuration](configuration.md) before a production deployment.
