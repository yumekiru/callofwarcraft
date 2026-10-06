# Patched local server

Use the `sources/vmangos` tree prepared by `scripts/Prepare-Sources.ps1`. It includes the custom bullet/NPC-control protocol, direct auto-loot and bag setup changes required by this client.

Follow the [vMaNGOS setup documentation](https://github.com/vmangos/wiki) for its supported database, dependencies, world database and build steps. Build this patched source tree rather than downloading an unmodified server binary. Use the configuration templates belonging to this source revision and enter your own local database credentials.

Generate the required `maps`, `vmaps` and `mmaps` using the server's extraction tools against your own compatible Warcraft installation. Store them locally; no generated map assets or databases are included here. Configure `DataDir` to point at that local server-data directory. Do not publish it.

Set these values in your local `mangosd.conf`, alongside the normal server configuration:

```ini
AutoAttackReach = 40
vmap.enableLOS = 1
vmap.enableHeight = 1
vmap.enableIndoorCheck = 1
```

Configure the local realm address and client build for 1.12.1. Start the database, `realmd` and `mangosd`, and create your own account and character using the server's ordinary setup procedure. The client launcher expects `127.0.0.1:3724` by default; update `WoWHost` when necessary.

The optional launcher fields `RealmServerExe`, `WorldServerExe` and `ServerWorkingDirectory` can start these two already-configured services. The launcher does not install a database, import dumps, generate game data or create accounts. Local configuration files and credentials are not part of this repository.

The custom NPC controller is intended for a trusted local experiment. Its client-driven movement/shooting protocol has not been audited as a secure public multiplayer server.
