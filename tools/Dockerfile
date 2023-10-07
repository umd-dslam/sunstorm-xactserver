FROM ctring/neon
USER root

RUN apt-get update
RUN apt-get install -y \
    libpq-dev \
    pkg-config \
    build-essential \
    zlib1g-dev \
    libncurses5-dev \
    libgdbm-dev \ 
    libnss3-dev \
    libssl-dev \
    libreadline-dev \
    libffi-dev \
    libsqlite3-dev \
    libbz2-dev \
    wget

WORKDIR /tmp
RUN wget https://www.python.org/ftp/python/3.12.0/Python-3.12.0.tar.xz
RUN tar -xf Python-3.12.0.tar.xz

WORKDIR /tmp/Python-3.12.0
RUN ./configure --enable-optimizations
RUN make -j 4 && make altinstall

RUN wget -P /usr/local/bin/minio https://dl.minio.io/server/minio/release/linux-amd64/minio
RUN chmod +x /usr/local/bin/minio

WORKDIR /tools
COPY *.py .
COPY requirements.txt .
RUN pip3.12 install -r requirements.txt

RUN mkdir -p /home/neon
RUN chown neon:neon /home/neon
USER neon
WORKDIR /home/neon

ENTRYPOINT ["python3.12", \
            "/tools/neonctl.py", \
            "--no-xactserver",\
            "--neon-dir", "/usr/local/bin", \
            "--pg-dir", "/usr/local"]

VOLUME ["/mr-data"]