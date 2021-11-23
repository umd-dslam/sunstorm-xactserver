FROM ubuntu

RUN apt-get update
RUN apt-get install -y build-essential vim git wget
RUN apt-get install -y libreadline-dev zlib1g-dev flex bison
RUN apt-get install -y curl 
RUN curl https://sh.rustup.rs -sSf | bash -s -- -y
RUN apt-get install -y cargo