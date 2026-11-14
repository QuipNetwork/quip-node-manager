# Trimming the quip-data directory

Directories from earlier network launches and old Docker images hold most of
the space you can get back without stopping the stack. Leave the live
validator database in place. It is often the largest item, and only a full
resync makes it smaller.

The examples use `~/quip-data`, the default data directory. If you chose a
different directory in Settings, use that path. On Windows the default is
`%USERPROFILE%\quip-data`.

## Measure first

```sh
du -sh ~/quip-data/* ~/quip-data/data/* ~/quip-data/.[!.]* 2>/dev/null | sort -h
docker system df
```

## Safe to delete while the stack runs

The current manager release uses only the runtime 120 directories. Each relaunch of the Aglais network gets a new pair of
directories, and the manager leaves the old pair on disk.

| Path | What it holds | Notes |
|------|---------------|-------|
| `data/aglais-chain-db/` | Validator database from before the runtime 119 relaunch | That chain no longer exists |
| `data/aglais-119-chain-db/` | Validator database from the runtime 119 relaunch | That chain no longer exists |
| `dashboard-data/` | Dashboard index from before runtime 119 | |
| `dashboard-data-119/` | Dashboard index from runtime 119 | |
| `.v0.1_backup/` | Files moved aside by the v0.1 to v0.2 upgrade | Keep it if you still want the v0.1 configuration |
| `.env.v0.1_backup` | The `.env` file from before the v0.2 upgrade | Same as the previous row |

Before you delete anything, make sure the stack uses the runtime 120 paths:

```sh
grep -E 'aglais-120-chain-db|dashboard-data-120' ~/quip-data/docker-compose.yml
```

You should see both names. Then remove the old directories:

```sh
cd ~/quip-data
rm -rf data/aglais-chain-db data/aglais-119-chain-db dashboard-data dashboard-data-119
rm -rf .v0.1_backup .env.v0.1_backup
```

The validator container writes files as your user (`PUID`/`PGID`). If an
older release wrote some files as root, `rm` shows the error
`Permission denied`. In that case, run the same command with `sudo`.

## Docker images

Each update pulls new image tags and leaves the old tags in the local Docker
cache. The manager never removes them. List the Quip images:

```sh
docker images --format '{{.Repository}}:{{.Tag}}  {{.Size}}' | grep quip.network
```

With the stack running, this command removes every image that no container
uses:

```sh
docker image prune -a
```

Run it only while the stack runs. If you run it with the stack stopped, it
also removes the current images. The next start then downloads them again,
and it fails if the registry does not answer. The command also removes unused
images of other projects on the machine. To keep other
projects' images, remove the old Quip tags one at a time with `docker rmi`.

## Logs need no cleanup

You do not need to clean up logs. Every log has a size limit:

| Log | Limit |
|-----|-------|
| `data/logs/quip-node.log` (the merged stack log) | 10 MB, plus 5 rotated files |
| Docker's per-container log cache | 32 MB × 5 per container, 8 MB × 2 for the dashboard |
| `node-output.log` (Native mode only) | Emptied at each start of the native miner |

## Keep these

| Path | Why |
|------|-----|
| `data/aglais-120-chain-db/` | The live validator database. It also holds the validator keystore and the libp2p node key |
| `dashboard-data-120/` | The live dashboard index. If you delete it, the dashboard indexes the chain again from genesis |
| `data/keystore.json` (Docker) or `keystore.json` (Native) | The miner's signing identity. If you lose it, you lose the funded, registered miner account |
| `node-secret.json` | The node secret. The start checklist requires it |
| `app-settings.json` | Your settings |
| `data/attempts/` (Docker) or `attempts/` (Native) | The miner's attempt history, one directory per block. The dashboard shows it |
| Docker volumes `quip_caddy-data` and `quip_caddy-config` | TLS certificates. If you delete them, Caddy requests new certificates and can reach the issuer's rate limits |

The manager writes `docker-compose.yml`, `.env`, `config.toml`, and
`chain-specs/` again at every start. Deleting them gets back almost no space.

### The miner attempt history

`data/attempts/` grows for as long as the miner runs, and nothing removes old
entries. If it becomes large, stop the stack and delete the oldest block
directories. The dashboard then shows no attempts for those blocks. Mining is
not affected.

### The validator database

The validator runs as an archive node (`--state-pruning=archive` and
`--blocks-pruning=archive`). It keeps every block and every state, so it
grows for as long as the chain runs. Do not change these flags to save space:

- Substrate does not open an archive database in a pruned mode. You must
  delete the database and sync again from genesis, which takes hours.
- The dashboard reads historical state back to genesis. On a pruned node,
  those reads fail with the error `State already discarded`.

If the database outgrows the disk, move the whole data directory to a larger
disk. Stop the stack, copy the directory, select the new path in Settings,
and start the stack again.
