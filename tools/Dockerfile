FROM ctring/neon
USER root

RUN apt-get update
RUN apt-get install -y python3 python3-pip

WORKDIR /tools
COPY *.py .
COPY requirements.txt .
RUN pip3 install -r requirements.txt
RUN curl https://dl.minio.io/server/minio/release/linux-amd64/minio -o /usr/local/bin/minio
RUN chmod +x /usr/local/bin/minio

RUN mkdir -p /home/neon
RUN chown neon:neon /home/neon
USER neon

WORKDIR /home/neon

ENTRYPOINT ["python3", \
            "/tools/neonctl.py", \
            "--no-xactserver",\
            "--neon-dir", "/usr/local/bin", \
            "--pg-dir", "/usr/local"]

VOLUME ["/mr-data"]