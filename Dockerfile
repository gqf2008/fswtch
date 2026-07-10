FROM rust:1-bookworm

ARG DEBIAN_FRONTEND=noninteractive
ARG FS_PREFIX=/usr/local/freeswitch

ENV FS_PREFIX=${FS_PREFIX}
ENV LD_LIBRARY_PATH=${FS_PREFIX}/lib
ENV PATH=${FS_PREFIX}/bin:${PATH}

RUN apt-get update && apt-get install -y --no-install-recommends \
    autoconf \
    automake \
    bison \
    build-essential \
    ca-certificates \
    clang \
    git \
    libcurl4-openssl-dev \
    libedit-dev \
    libncurses-dev \
    libpcre2-dev \
    libspeex-dev \
    libspeexdsp-dev \
    libsqlite3-dev \
    libssl-dev \
    libtiff-dev \
    libtool \
    libtool-bin \
    nasm \
    pkg-config \
    uuid-dev \
    zlib1g-dev \
    && rm -rf /var/lib/apt/lists/*

RUN set -eux; \
    mkdir -p /usr/src/libs; \
    git clone --depth 1 https://github.com/freeswitch/sofia-sip /usr/src/libs/sofia-sip; \
    cd /usr/src/libs/sofia-sip; \
    ./bootstrap.sh; \
    ./configure --with-pic --with-glib=no --without-doxygen --disable-stun --prefix=/usr; \
    make -j"$(nproc)"; \
    make install; \
    git clone --depth 1 https://github.com/freeswitch/spandsp /usr/src/libs/spandsp; \
    cd /usr/src/libs/spandsp; \
    ./bootstrap.sh; \
    ./configure --with-pic --prefix=/usr; \
    make -j"$(nproc)"; \
    make install; \
    ldconfig

WORKDIR /usr/src/switch-sys
COPY . .

RUN git clone https://github.com/signalwire/freeswitch

RUN set -eux; \
    cd freeswitch; \
    printf '%s\n' \
      applications/mod_commands \
      event_handlers/mod_event_socket \
      loggers/mod_console \
      loggers/mod_logfile \
      > build/modules.conf.in; \
    ./bootstrap.sh -j; \
    ./configure --prefix="${FS_PREFIX}"; \
    make -j"$(nproc)"; \
    make install; \
    ln -s lib/freeswitch/mod "${FS_PREFIX}/mod"

RUN set -eux; \
    FREESWITCH_INCLUDE_DIR="${FS_PREFIX}/include/freeswitch" \
    FREESWITCH_LIB_DIR="${FS_PREFIX}/lib" \
    cargo build -p fswtch --examples --release; \
    for module in \
      mod_api_suite \
      mod_async_job_queue \
      mod_app_playback_control \
      mod_cdr_enricher \
      mod_chatbot_bridge \
      mod_config_xml \
      mod_endpoint_skeleton \
      mod_event_sink \
      mod_hello \
      mod_http_webhook \
      mod_lifecycle \
      mod_local_ai_bridge \
      mod_media_bug_meter \
      mod_metrics \
      mod_rate_limiter \
      mod_registration_check \
      mod_remote_vad \
      mod_stream_tools \
      mod_vad_esl; \
    do \
      install -m 0755 "target/release/examples/lib${module}.so" "${FS_PREFIX}/mod/${module}.so"; \
    done

COPY docker/fswtch/conf/ ${FS_PREFIX}/conf/
COPY docker/fswtch/bin/verify-fswtch-examples /usr/local/bin/verify-fswtch-examples
RUN chmod +x /usr/local/bin/verify-fswtch-examples

CMD ["verify-fswtch-examples"]
