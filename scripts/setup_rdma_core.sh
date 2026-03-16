#!/bin/bash
set -e

# Change to the project root directory (one level up from scripts/)
cd "$(dirname "$0")/.."

echo "Setting up rdma-core environment..."

# 1. Clone the official linux-rdma/rdma-core repository if it doesn't exist
if [ ! -d "rdma-core" ]; then
    echo "Cloning rdma-core..."
    git clone https://github.com/linux-rdma/rdma-core.git
    echo "Successfully cloned rdma-core."
else
    echo "rdma-core directory already exists, skipping clone."
fi

# 2. Build rdma-core using its built-in build script
echo "Building rdma-core..."
cd rdma-core
./build.sh
echo "Successfully built rdma-core."

echo ""
echo "Done."
echo "Note: To install the built libraries to your system, run:"
echo "  cd rdma-core/build && sudo make install"
