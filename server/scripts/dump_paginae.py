#!/usr/bin/env python3
"""Session 41 wire dump: capture RMSG_PAGINAE entries verbatim (autonomous)."""
import socket, ssl, struct, sys, time

PORT_AUTH = 1871
PORT_GAME = 1870


def auth_cookie(username, password="x", port=PORT_AUTH):
    import hashlib
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE
    raw = socket.create_connection(("127.0.0.1", port), timeout=5)
    tls = ctx.wrap_socket(raw)
    usr = username.encode()
    digest = hashlib.sha256(password.encode()).digest()

    def send_frame(ty, payload):
        tls.sendall(bytes([ty, len(payload)]) + payload)

    def recv_frame():
        head = b""
        while len(head) < 2:
            c = tls.recv(2 - len(head))
            if not c:
                raise RuntimeError("eof")
            head += c
        ln = head[1]
        body = b""
        while len(body) < ln:
            body += tls.recv(ln - len(body))
        return head[0], body

    send_frame(1, usr)
    ty, _ = recv_frame()
    assert ty == 0, "CMD_USR rejected"
    send_frame(2, digest)
    ty, body = recv_frame()
    assert ty == 0, f"CMD_PASSWD rejected {ty}"
    return body


def main():
    user = sys.argv[1] if len(sys.argv) > 1 else "pagsniff"
    pw = sys.argv[2] if len(sys.argv) > 2 else "x"
    cookie = auth_cookie(user, pw)
    if not cookie:
        print("no cookie")
        return
    s = socket.create_connection(("127.0.0.1", PORT_GAME), timeout=5)
    s.sendall(b"hlauhunk" + cookie)
    s.settimeout(4)
    data = b""
    try:
        while len(data) < (1 << 21):
            chunk = s.recv(65536)
            if not chunk:
                break
            data += chunk
    except socket.timeout:
        pass
    needle = b"paginae/atk/blk\x00"
    idx = data.find(needle)
    print("blk name offset:", idx)
    if idx >= 0:
        ver_off = idx + len(needle)
        print("bytes after name:", data[ver_off:ver_off + 4].hex())
    # dump ALL paginae entries: scan for '+<name>...' inside type-5 frames is
    # unreliable on a raw stream; report every atk pagina name + trailing u16
    for name in [b"paginae/atk/atk\x00", b"paginae/atk/pow\x00", b"paginae/atk/dodge\x00"]:
        i = data.find(name)
        if i >= 0:
            v = struct.unpack("<H", data[i + len(name):i + len(name) + 2])[0]
            print(name.decode().rstrip("\x00"), "ver:", v, "hex:", data[i + len(name):i + len(name) + 2].hex())
        else:
            print(name.decode().rstrip("\x00"), "NOT FOUND in stream")


if __name__ == "__main__":
    main()
