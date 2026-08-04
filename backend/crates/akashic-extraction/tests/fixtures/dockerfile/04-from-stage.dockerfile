FROM alpine:3.20 AS base
RUN apk add --no-cache ca-certificates

FROM base AS a
RUN echo "stage a"

FROM a AS b
RUN echo "stage b derived from a"
CMD ["sh"]
