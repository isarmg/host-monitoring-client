"""One bounded test-only TLS connection, using OpenSSL independently of reqwest.

stdout contains only readiness and a non-secret result; never echo request data.
The Rust owner kills/reaps this process on assertion failure or deadline.
"""
import datetime
import gzip
import io
import json
import pathlib
import socket
import ssl
import sys

root, certificate, require_client, version, target = sys.argv[1:]
root = pathlib.Path(root)
context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
context.minimum_version = context.maximum_version = getattr(ssl.TLSVersion, version)
context.load_cert_chain(root / f"{certificate}.crt", root / f"{certificate}.key")
if require_client == "yes":
    context.verify_mode = ssl.CERT_REQUIRED
    context.load_verify_locations(root / "ca.crt")

with socket.socket() as listener:
    listener.bind(("127.0.0.1", 0))
    listener.listen(1)
    listener.settimeout(8)
    print(json.dumps({"port": listener.getsockname()[1]}), flush=True)
    raw, _ = listener.accept()
    raw.settimeout(5)
    result = {"handshake": False, "http": False, "client_authenticated": False}
    try:
        with context.wrap_socket(raw, server_side=True) as connection:
            result["handshake"] = True
            result["version"] = connection.version()
            peer = connection.getpeercert()
            if require_client == "yes":
                names = [value for rdn in peer["subject"] for key, value in rdn if key == "commonName"]
                assert names == ["host-client-fixture"], "unexpected client certificate"
                result["client_authenticated"] = True
            request = bytearray()
            while b"\r\n\r\n" not in request:
                chunk = connection.recv(4096)
                assert chunk and len(request) + len(chunk) <= 16 * 1024, "invalid HTTP headers"
                request.extend(chunk)
            headers, body = bytes(request).split(b"\r\n\r\n", 1)
            lines = headers.decode("ascii").split("\r\n")
            expected_path = "/v1/metrics" if target == "otlp" else "/api/v2/host-monitor/report"
            assert lines[0] == f"POST {expected_path} HTTP/1.1", "unexpected method or target"
            fields = {}
            for line in lines[1:]:
                name, value = line.split(":", 1)
                name = name.lower()
                assert name not in fields, "duplicate header"
                fields[name] = value.strip()
            assert fields.get("authorization") == "Bearer tls-fixture-secret-marker", "missing or incorrect authorization"
            length = int(fields["content-length"])
            assert 0 < length <= 1024 * 1024, "invalid body length"
            while len(body) < length:
                chunk = connection.recv(min(4096, length - len(body)))
                assert chunk, "incomplete body"
                body += chunk
            assert len(body) == length, "unexpected pipelined data"
            if target == "otlp":
                assert fields["content-type"] == "application/x-protobuf", "invalid OTLP type"
                assert fields["content-encoding"] == "gzip", "invalid OTLP encoding"
                with gzip.GzipFile(fileobj=io.BytesIO(body)) as decoder:
                    decoded = decoder.read(1024 * 1024 + 1)
                assert 0 < len(decoded) <= 1024 * 1024, "invalid OTLP message size"
                response = b"{}"
                status = b"200 OK"
            else:
                assert fields["content-type"] == "application/json", "invalid report type"
                report = json.loads(body)
                result["report_id"] = report["report_id"]
                result["host_id"] = report["host"]["id"]
                response = json.dumps({
                    "host_id": report["host"]["id"],
                    "report_id": report["report_id"],
                    "accepted": True,
                    "received_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
                }).encode()
                status = b"202 Accepted"
            result["http"] = True
            connection.sendall(b"HTTP/1.1 " + status + b"\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: "
                               + str(len(response)).encode() + b"\r\n\r\n" + response)
    except ssl.SSLError:
        result["tls_rejected"] = True
    finally:
        raw.close()
    print(json.dumps(result), flush=True)
