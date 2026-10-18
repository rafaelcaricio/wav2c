#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
PORT=${PORT:-9090}
DIR=${DIR:-$ROOT/web/multiview_wasm}
CERT_DIR=${CERT_DIR:-$ROOT/target/demo/tls}
mkdir -p "$CERT_DIR"

if [ ! -f "$CERT_DIR/cert.pem" ]; then
  IPS=$(hostname -I 2>/dev/null || true)
  SAN="DNS:localhost,IP:127.0.0.1"
  for ip in $IPS; do
    case "$ip" in
      *:*) ;;
      *) SAN="$SAN,IP:$ip" ;;
    esac
  done
  openssl req -x509 -newkey rsa:2048 -nodes -days 825 \
    -keyout "$CERT_DIR/key.pem" -out "$CERT_DIR/cert.pem" \
    -subj "/CN=av2codec-multiview-demo" \
    -addext "subjectAltName=$SAN" >/dev/null 2>&1
  echo "generated self-signed certificate in $CERT_DIR (SAN: $SAN)"
fi

echo "serving $DIR"
hostname -I 2>/dev/null | tr ' ' '\n' | grep -v '^$' | sed "s|^|  https://|; s|$|:$PORT/|" || true
echo "  open the HTTPS URL, accept the certificate warning, then Enable camera"

python3 - "$DIR" "$PORT" "$CERT_DIR" <<'PY'
import http.server, ssl, sys, functools

directory, port, cert_dir = sys.argv[1], int(sys.argv[2]), sys.argv[3]
handler = functools.partial(http.server.SimpleHTTPRequestHandler, directory=directory)
server = http.server.ThreadingHTTPServer(("0.0.0.0", port), handler)
context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
context.load_cert_chain(f"{cert_dir}/cert.pem", f"{cert_dir}/key.pem")
server.socket = context.wrap_socket(server.socket, server_side=True)
print(f"listening on https://0.0.0.0:{port}/")
server.serve_forever()
PY
