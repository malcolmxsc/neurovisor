# NeuroVisor: AI Agent Sandbox with Firecracker VMs

**NeuroVisor** is a secure AI agent execution platform built in Rust, using Firecracker microVMs for isolation. It provides an agent loop that uses LLMs (via Ollama) for code generation and executes code in sandboxed VMs.

## Architecture

```
User Task → AgentController → Ollama /api/chat (with tools)
                ↓
         Tool Call: execute_code
                ↓
         VMPool.acquire() → Pre-warmed VM
                ↓
         ExecutionClient (vsock) → Guest ExecutionServer
                ↓
         Code runs in VM → stdout/stderr/exit_code
                ↓
         VMPool.release() (destroy VM)
                ↓
         Feed result back to Ollama → Loop or Complete
```

## Features

### Core Virtualization
- **Pre-warmed VM Pool**: VMs boot in background, ready for instant assignment
- **Sub-second Boot**: Custom kernel configuration achieves <500ms boot times
- **Isolation**: Each request gets its own VM, destroyed after use
- **Resource Limits**: cgroups v2 for CPU/memory limits per VM

### Agent Capabilities
- **Ollama Integration**: Uses `/api/chat` with tool calling support
- **Code Execution**: Python, Bash, JavaScript runtimes in guest
- **Multi-turn Conversations**: Agent loops until task completion
- **Trace Correlation**: End-to-end distributed tracing with trace IDs

### Security
- **Seccomp Filters**: Restricted syscalls for Firecracker process
- **Capability Dropping**: Minimal Linux capabilities
- **Rate Limiting**: Token bucket rate limiter for requests
- **eBPF LSM**: File access enforcement (on supported kernels)

### Observability
- **Prometheus Metrics**: VM boot time, request latency, pool status
- **eBPF Tracing**: Syscall counts, process lifecycle events
- **Distributed Tracing**: trace_id propagation through gRPC metadata
- **Pushgateway Support**: Push metrics to Prometheus Pushgateway

## Quick Start

### Prerequisites

- Linux host with KVM access (`/dev/kvm`)
- Rust toolchain (1.75+)
- Ollama running locally (`ollama serve`)
- Firecracker binary in PATH
- Pre-built kernel (`vmlinux`) and rootfs (`rootfs.ext4`)

### Building

```bash
# Build the main binary
cargo build --release

# Build guest agent (for rootfs)
cargo build --release --bin guest_agent --target x86_64-unknown-linux-musl

# Build with eBPF support (optional)
./build-ebpf.sh
cargo build --release --features ebpf
```

### Running

#### Agent Mode (Interactive AI)

```bash
# Run an agent task
sudo ./target/release/neurovisor --agent "Find all prime numbers under 100"

# With Pushgateway for metrics
sudo ./target/release/neurovisor --agent "task" --pushgateway http://localhost:9091
```

#### Server Mode (gRPC Gateway)

```bash
# Start the gRPC server
sudo ./target/release/neurovisor

# Server listens on:
# - gRPC: 0.0.0.0:50051
# - Metrics: 0.0.0.0:9090/metrics
```

### gRPC Client Example

```bash
# Using grpcurl
grpcurl -plaintext -d '{"prompt": "Hello, world!", "model": "llama3.2"}' \
    localhost:50051 inference.InferenceService/Infer

# With trace ID
grpcurl -plaintext \
    -H 'x-trace-id: my-trace-123' \
    -d '{"prompt": "Hello!", "model": "llama3.2"}' \
    localhost:50051 inference.InferenceService/Infer
```

## Configuration

### VM Sizes

| Size   | vCPUs | Memory | Use Case |
|--------|-------|--------|----------|
| Small  | 1     | 256MB  | Simple scripts |
| Medium | 2     | 512MB  | Standard workloads |
| Large  | 4     | 1024MB | Complex tasks |

### Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `OLLAMA_HOST` | `http://localhost:11434` | Ollama API endpoint |
| `NEUROVISOR_POOL_SIZE` | `3` | Number of pre-warmed VMs |
| `NEUROVISOR_MAX_VMS` | `10` | Maximum concurrent VMs |

## Metrics

Key Prometheus metrics:

| Metric | Type | Description |
|--------|------|-------------|
| `neurovisor_vm_boot_duration_seconds` | Histogram | VM boot time |
| `neurovisor_pool_warm_vms` | Gauge | VMs ready in pool |
| `neurovisor_pool_active_vms` | Gauge | VMs currently in use |
| `neurovisor_requests_total` | Counter | Total inference requests |
| `neurovisor_inference_duration_seconds` | Histogram | Inference latency |
| `neurovisor_agent_iterations` | Histogram | Agent loop iterations |
| `neurovisor_ebpf_syscall_total` | Counter | Syscalls traced per VM |

## Project Structure

```
neurovisor/
├── src/
│   ├── agent/          # Agent controller and loop logic
│   ├── cgroups/        # Resource limit management
│   ├── ebpf/           # eBPF tracing and security
│   ├── grpc/           # gRPC server and clients
│   ├── metrics/        # Prometheus metrics
│   ├── ollama/         # Ollama client (generate + chat)
│   ├── security/       # Seccomp, rate limiting
│   └── vm/             # VM manager, pool, handle
├── guest/
│   └── agent/          # In-VM execution server
├── ebpf-programs/      # eBPF programs (syscall, LSM, tracing)
├── proto/              # gRPC service definitions
└── docs/               # Architecture documentation
```

## Development

```bash
# Run tests
cargo test

# Run with debug logging
RUST_LOG=debug cargo run

# Check formatting
cargo fmt --check

# Run clippy
cargo clippy
```

## License

MIT
