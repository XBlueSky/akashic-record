FROM ubuntu:22.04
# Install build dependencies in a single layer
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        build-essential \
        curl \
    && rm -rf /var/lib/apt/lists/*
ENV PATH=/usr/local/bin:$PATH
CMD ["bash"]
