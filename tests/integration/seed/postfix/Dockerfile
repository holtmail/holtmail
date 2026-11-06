FROM alpine:3.22
RUN apk add --no-cache postfix ca-certificates
COPY main.cf /etc/postfix/main.cf
EXPOSE 25
CMD ["/bin/sh", "-c", "cp /certs/ca.crt /etc/postfix/stack-ca.pem && newaliases && exec postfix start-fg"]
