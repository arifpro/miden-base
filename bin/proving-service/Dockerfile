# Stage 1: Build Stage
FROM rust:1.75 AS builder

# Install required dependencies for building
RUN apt-get update && apt-get install -y \
    cmake \
    build-essential \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

# Set working directory
WORKDIR /app

# Copy source code
COPY . .

# Build the binary in release mode
RUN cargo build --release

# Stage 2: Final Runtime Image
FROM debian:bullseye-slim

# Install required runtime dependencies for OpenSSL 3 and build tools
RUN apt-get update && apt-get install -y \
    wget \
    make \
    gcc \
    perl \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Download and install OpenSSL 3.0 manually
RUN wget https://www.openssl.org/source/openssl-3.0.7.tar.gz && \
    tar -xvf openssl-3.0.7.tar.gz && \
    cd openssl-3.0.7 && \
    ./config && \
    make && \
    make install && \
    cd .. && \
    rm -rf openssl-3.0.7.tar.gz openssl-3.0.7

# Set working directory for binary
WORKDIR /usr/local/bin

# Check if the target folder exists, log the result, and copy the compiled binary
RUN echo "Checking if /usr/local/bin exists..." && \
    if [ ! -d "/usr/local/bin" ]; then \
        echo "/usr/local/bin does not exist, creating it."; \
        mkdir -p /usr/local/bin; \
    else \
        echo "/usr/local/bin exists."; \
    fi

# Copy the compiled binary from the builder stage
COPY --from=builder /app/target/release/miden-proving-service .

# Ensure the binary is executable
RUN chmod +x /usr/local/bin/miden-proving-service

# Log the binary location and verify it's copied correctly
RUN echo "Checking if the binary exists in /usr/local/bin..." && \
    ls -l /usr/local/bin/miden-proving-service && \
    /usr/local/bin/miden-proving-service --help || true

# Expose the service port
EXPOSE 8082

# Initialize the service and run
RUN /usr/local/bin/miden-proving-service init

# Set the entrypoint to the service start command
ENTRYPOINT ["/usr/local/bin/miden-proving-service"]
CMD ["start-proxy", "0.0.0.0:8082"]
