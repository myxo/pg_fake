FROM rust:bookworm

RUN apt-get update && apt-get install -y --no-install-recommends \
    clang cmake flex bison libclang-dev libpq-dev libyaml-cpp-dev \
    llvm-dev make ninja-build pkg-config zlib1g-dev \
    && rm -rf /var/lib/apt/lists/*

RUN curl -fsSL https://www.postgresql.org/media/keys/ACCC4CF8.asc \
      -o /usr/share/keyrings/postgresql.asc \
    && echo "deb [signed-by=/usr/share/keyrings/postgresql.asc] http://apt.postgresql.org/pub/repos/apt bookworm-pgdg main" \
      > /etc/apt/sources.list.d/pgdg.list \
    && apt-get update \
    && apt-get install -y --no-install-recommends postgresql-18 \
    && rm -rf /var/lib/apt/lists/*

ARG SQUIRREL_REV=96810fd96cfb40fe8d899e8d7a4f3eaae191d1db
RUN git clone https://github.com/s3team/Squirrel /opt/squirrel-src \
    && git -C /opt/squirrel-src checkout ${SQUIRREL_REV} \
    && git -C /opt/squirrel-src submodule update --init AFLplusplus third_party/abseil-cpp

RUN make -C /opt/squirrel-src/AFLplusplus source-only -j"$(nproc)" \
    && test -f /opt/squirrel-src/AFLplusplus/afl-compiler-rt.o

RUN cmake -S /opt/squirrel-src -B /opt/squirrel-src/build \
      -DCMAKE_BUILD_TYPE=Release -DPOSTGRESQL=ON -Wno-dev -G Ninja \
    && cmake --build /opt/squirrel-src/build -j"$(nproc)"

RUN install -d -o postgres /var/lib/postgresql/data \
    && su postgres -s /bin/bash -c "/usr/lib/postgresql/18/bin/initdb \
      -D /var/lib/postgresql/data --auth-local=trust --auth-host=trust"

WORKDIR /workspace
COPY . .
RUN chmod +x fuzz/squirrel/rustc_wrapper.sh
RUN cd fuzz && RUSTC_WRAPPER=/workspace/fuzz/squirrel/rustc_wrapper.sh RUSTFLAGS="\
--cfg fuzzing \
-C debug-assertions -C overflow-checks \
-C passes=sancov-module \
-C llvm-args=-sanitizer-coverage-level=3 \
-C llvm-args=-sanitizer-coverage-trace-pc-guard \
-C llvm-args=-sanitizer-coverage-prune-blocks=0 \
-C link-arg=/opt/squirrel-src/AFLplusplus/afl-compiler-rt.o" \
    cargo build --release --bin squirrel_matches_postgres \
    && install -D target/release/squirrel_matches_postgres \
      /opt/fuzz-bin/squirrel_matches_postgres

COPY fuzz/squirrel/seeds /opt/seeds
COPY fuzz/squirrel/config.yml /opt/squirrel-config.yml
COPY fuzz/squirrel/entrypoint.sh /opt/entrypoint.sh

ENTRYPOINT ["/opt/entrypoint.sh"]
