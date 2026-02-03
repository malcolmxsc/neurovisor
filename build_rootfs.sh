#!/bin/bash
# Build enhanced rootfs with Python, pip, and Rust
set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

echo "=== Building Enhanced Rootfs ==="
echo "This will create rootfs.ext4 with Python, pip, and Rust"
echo ""

# First, ensure guest_agent is built for musl (static linking)
echo "[1/5] Building guest_agent for musl target..."
if ! rustup target list --installed | grep -q x86_64-unknown-linux-musl; then
    echo "Installing musl target..."
    rustup target add x86_64-unknown-linux-musl
fi

cargo build --release --target x86_64-unknown-linux-musl -p guest_agent 2>/dev/null || {
    echo "Note: guest_agent build may have warnings, continuing..."
}

# Check if the binary exists
GUEST_AGENT="target/x86_64-unknown-linux-musl/release/guest_agent"
if [ ! -f "$GUEST_AGENT" ]; then
    echo "Error: guest_agent binary not found at $GUEST_AGENT"
    echo "Trying debug build..."
    cargo build --target x86_64-unknown-linux-musl -p guest_agent
    GUEST_AGENT="target/x86_64-unknown-linux-musl/debug/guest_agent"
fi

echo "[2/5] Creating Docker container with Python, pip, and Rust..."

# Create a Dockerfile for the rootfs
cat > /tmp/Dockerfile.rootfs << 'EOF'
FROM alpine:3.19

# Install Python 3, pip, and common packages
RUN apk add --no-cache \
    python3 \
    py3-pip \
    rust \
    cargo \
    bash \
    coreutils \
    && pip3 install --break-system-packages \
    requests \
    && rm -rf /var/cache/apk/* /root/.cache

# Create directories for guest agent
RUN mkdir -p /usr/local/bin

# The guest_agent will be copied in later
EOF

# Build the Docker image
docker build -t neurovisor-rootfs -f /tmp/Dockerfile.rootfs .

echo "[3/5] Extracting filesystem from container..."

# Create container and export filesystem
CONTAINER_ID=$(docker create neurovisor-rootfs)
docker export "$CONTAINER_ID" > /tmp/rootfs.tar
docker rm "$CONTAINER_ID" > /dev/null

echo "[4/5] Creating ext4 filesystem image..."

# Create working directory
WORK_DIR=$(mktemp -d)
cd "$WORK_DIR"

# Extract tarball
mkdir rootfs
tar -xf /tmp/rootfs.tar -C rootfs

# Copy guest_agent binary
cp "$SCRIPT_DIR/$GUEST_AGENT" rootfs/usr/local/bin/guest_agent
chmod +x rootfs/usr/local/bin/guest_agent

# Copy run_guest.sh init script
cp "$SCRIPT_DIR/run_guest.sh" rootfs/usr/local/bin/run_guest.sh
chmod +x rootfs/usr/local/bin/run_guest.sh

# Create ext4 image (1GB, same as original)
dd if=/dev/zero of=rootfs.ext4 bs=1M count=1024 status=progress
mkfs.ext4 -d rootfs rootfs.ext4

# Move to final location
mv rootfs.ext4 "$SCRIPT_DIR/rootfs.ext4"

echo "[5/5] Cleaning up..."
cd "$SCRIPT_DIR"
rm -rf "$WORK_DIR" /tmp/rootfs.tar /tmp/Dockerfile.rootfs

echo ""
echo "=== Done! ==="
echo "New rootfs.ext4 created with:"
echo "  - Python 3 + pip"
echo "  - Rust + Cargo"
echo "  - Bash + coreutils"
echo "  - guest_agent binary"
echo ""
echo "Size: $(du -h rootfs.ext4 | cut -f1)"
