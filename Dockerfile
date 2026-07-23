# --- embedding model (baked in, so the container has no network dependency) ---
FROM debian:trixie-slim AS model
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*
ARG MODEL_REPO=Xenova/multilingual-e5-small
RUN mkdir -p /model \
    && curl -fsSL -o /model/model.onnx \
       "https://huggingface.co/${MODEL_REPO}/resolve/main/onnx/model_quantized.onnx" \
    && curl -fsSL -o /model/tokenizer.json \
       "https://huggingface.co/${MODEL_REPO}/resolve/main/tokenizer.json"

# --- build ---
FROM rust:1-trixie AS builder
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked

# --- runtime ---
# trixie, not bookworm: the prebuilt ONNX Runtime that `ort` downloads needs
# glibc >= 2.38 (__isoc23_* symbols), which bookworm's 2.36 does not have.
FROM debian:trixie-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends zlib1g libgomp1 \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /src/target/release/kyb-server /usr/local/bin/kyb-server
COPY --from=model /model /model

# /data — external directory: the git canon, index and audit log live on the host
ENV KYB_DATA=/data/kyb-data \
    KYB_INDEX=/data/index \
    KYB_AUDIT=/data/audit.jsonl \
    KYB_MODEL=/model \
    KYB_ADDR=0.0.0.0:9310
VOLUME /data
EXPOSE 9310
CMD ["kyb-server"]
