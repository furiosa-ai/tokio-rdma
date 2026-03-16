#!/bin/bash
set -e

# Change to the project root directory (one level up from scripts/)
cd "$(dirname "$0")/.."

echo "Setting up rdma-core environment..."

# 1. Clone
if [ ! -d "rdma-core" ]; then
    ./scripts/clone_rdma_core.sh
else
    echo "rdma-core already exists, skipping clone."
fi

# 2. Build
./scripts/build_rdma_core.sh

echo "Done."
