FROM --platform=linux/amd64 rust:1.98-slim-trixie


# C toolchain required by rusqlite's "bundled" feature (compiles SQLite from source).
# python3-pip + ziglang provide the `zig` compiler used by cargo-zigbuild to
# link against a specific minimum glibc version.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
       build-essential vim hunspell-en-us python3-pip \
    && pip3 install ziglang --break-system-packages \
    && rm -rf /var/lib/apt/lists/*

# Install cargo-zigbuild into the image's default CARGO_HOME (/usr/local/cargo)
# *before* we redirect CARGO_HOME to the runtime volume below, so the binary
# persists in the image layer and is always available in PATH.
RUN cargo install cargo-zigbuild

# Pre-create the Cargo cache dir with open permissions so any host UID can write
# when the container is run as the host user (--user uid:gid).
RUN mkdir -p /cargo-cache && chmod 777 /cargo-cache
ENV CARGO_HOME=/cargo-cache
ENV XDG_CACHE_HOME=/cargo-cache/.cache
ENV XDG_DATA_HOME=/workspace/.data

WORKDIR /workspace
