FROM debian:bookworm@sha256:f37a335e82bca302e955fa39f9dfe28f1be618f016f8a2b56318e5a5111afc26

RUN apt-get update \
    && apt-get install -y --no-install-recommends libxml2 openssl tar gzip ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /work

COPY openssl.cnf /etc/chroma-wd-os5-openssl.cnf
ENV OPENSSL_CONF=/etc/chroma-wd-os5-openssl.cnf
