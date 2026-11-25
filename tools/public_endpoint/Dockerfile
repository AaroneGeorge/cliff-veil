# The public half of the read-only sequencer endpoint (relay.py). Standard library only: nothing is installed.
# The base image is pinned by digest (python:3.12-alpine, looked up 2026-11-20).
FROM python:3.12-alpine@sha256:c5ba3deef00c7758561a0f03c26f44f10589263d40e732b8748e0d1f38f6dd89
COPY policy.py relay.py /app/
USER 65534
EXPOSE 8080
CMD ["python3","-I","/app/relay.py"]
