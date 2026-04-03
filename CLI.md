# Running a Quip Node via Docker (CLI)

## 1. Create the data directory

```bash
mkdir -p ~/quip-data
```

## 2. Create `~/quip-data/config.toml`

Minimal config (adjust values as needed):

```toml
[global]
listen = "0.0.0.0"
port = 20049
# public_host = "YOUR_PUBLIC_IP"
auto_mine = true
genesis_config = "mainnet"
peer = ["qpu-1.nodes.quip.network:20049", "cpu-1.quip.carback.us:20049", "gpu-1.quip.carback.us:20049", "gpu-2.quip.carback.us:20050", "nodes.quip.network:20049"]
timeout = 30
heartbeat_interval = 30
heartbeat_timeout = 90
verify_tls = false
tofu = true
trust_db = "/data/trust.db"
log_level = "info"
node_log = "/data/node.log"
telemetry_enabled = false

[cpu]
num_cpus = 4
```

> **Note:** Paths in the config must use `/data/...` (the Docker mount point), not `~/quip-data/...`.

## 3. Pull the image

```bash
# CPU-only
docker pull registry.gitlab.com/quip.network/quip-protocol/quip-network-node-cpu:latest

# OR for CUDA GPU support
docker pull registry.gitlab.com/quip.network/quip-protocol/quip-network-node-cuda:latest
```

## 4. Remove any stale container

```bash
docker rm -f quip-node 2>/dev/null
```

## 5. Run the container

**CPU:**

```bash
docker run -d \
  --name quip-node \
  -p 20049:20049/udp \
  -p 20049:20049/tcp \
  -v ~/quip-data:/data \
  registry.gitlab.com/quip.network/quip-protocol/quip-network-node-cpu:latest
```

**CUDA GPU:**

```bash
docker run -d \
  --name quip-node \
  -p 20049:20049/udp \
  -p 20049:20049/tcp \
  -v ~/quip-data:/data \
  --gpus all \
  registry.gitlab.com/quip.network/quip-protocol/quip-network-node-cuda:latest
```

**With optional environment variables:**

```bash
docker run -d \
  --name quip-node \
  -p 20049:20049/udp \
  -p 20049:20049/tcp \
  -v ~/quip-data:/data \
  -e QUIP_PUBLIC_HOST=203.0.113.42 \
  -e QUIP_NODE_NAME=my-node \
  registry.gitlab.com/quip.network/quip-protocol/quip-network-node-cpu:latest
```

## 6. Open firewall and router ports

The node requires **both UDP and TCP** on port 20049 (or your configured port).

**Firewall (Linux/ufw):**

```bash
sudo ufw allow 20049/udp
sudo ufw allow 20049/tcp
```

**Router:** Forward both UDP and TCP for port 20049 to your machine's local IP.

## 7. Check status and logs

```bash
docker ps -f name=quip-node
docker logs -f --tail 100 quip-node 2>&1
```

## 8. Stop

```bash
docker stop quip-node
docker rm -f quip-node
```
