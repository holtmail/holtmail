FROM alpine:3.22
RUN apk add --no-cache postfix opendkim ca-certificates
COPY main.cf /etc/postfix/main.cf
COPY opendkim.conf /etc/opendkim/opendkim.conf
EXPOSE 25
CMD ["/bin/sh", "-c", "newaliases && mkdir -p /run/opendkim && chown opendkim /run/opendkim && opendkim -x /etc/opendkim/opendkim.conf && exec postfix start-fg"]
