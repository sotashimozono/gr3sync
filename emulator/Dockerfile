# A fake RICOH GR III's Wi-Fi side, serving the camera's HTTP API.
#
# The point of running this in a container rather than as a plain subprocess is
# the address: on its own bridge network it can hold 192.168.0.1:80, which is
# where the real camera lives and is hard-coded in its firmware. That removes
# the last "but the test used a different host" caveat from the Wi-Fi half.
#
# Build from the repository root:
#   docker build -f emulator/Dockerfile -t gr3-emulator .

FROM rust:1-slim AS build
WORKDIR /src

# libdbus is needed because btleplug is a dependency of the library crate, even
# though the emulator binary itself never touches Bluetooth.
RUN apt-get update \
 && apt-get install -y --no-install-recommends libdbus-1-dev pkg-config \
 && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --features emulator --bin gr3-emulator

FROM debian:stable-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends libdbus-1-3 \
 && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/gr3-emulator /usr/local/bin/gr3-emulator

EXPOSE 80
ENTRYPOINT ["/usr/local/bin/gr3-emulator", "serve"]
CMD ["--bind", "0.0.0.0:80", "--pairs", "5"]
