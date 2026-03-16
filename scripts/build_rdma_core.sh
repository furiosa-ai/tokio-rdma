#!/bin/bash
set -e

# Build rdma-core using its built-in build script
if [ ! -d "rdma-core" ]; then
    echo "rdma-core directory not found. Please run scripts/clone_rdma_core.sh first."
    exit 1
fi

echo "Building rdma-core..."
cd rdma-core
./build.sh
echo "Successfully built rdma-core."
echo "Note: To install the built libraries, run 'cd rdma-core/build && sudo make install'"
