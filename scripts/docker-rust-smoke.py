#!/usr/bin/env python3
"""Smoke-test the Rust-only Docker image (Dockerfile.rust) on Linux.

    python3 scripts/docker-rust-smoke.py pp-server:test

Checks, in order: no Node, npm, npx or Python in the runtime; the image user is
1000:1000; `pp-server healthcheck --help`; a hardened start (read-only root,
bounded /tmp, all capabilities dropped, no-new-privileges) on a fresh named
volume; sign-in through the production bootstrap-url command; liveness and readiness; an account written to SQLite and read back
after a restart; SIGTERM drains with a complete shutdown receipt and leaves no
process behind. The container publishes port 8080 on a free host loopback
port, so requests reach it from a non-loopback Docker bridge peer. Sign-in
URLs and cookies stay in memory and are never printed. Every container and
volume it creates is removed on exit.
"""

import http.client
import ipaddress
import json
import subprocess
import sys
import time
import urllib.parse
import uuid

STATUS = "/tmp/pp-server.status.json"
CREDENTIAL = "/tmp/pp-smoke-credential.json"


def docker(*args, check=True):
    result = subprocess.run(["docker", *args], capture_output=True, text=True)
    if check and result.returncode != 0:
        raise SystemExit(f"docker {args[0]} failed ({result.returncode}): {result.stderr.strip()}")
    return result


def log(message):
    print(f"[docker-smoke] {message}", flush=True)


def request(origin, method, path, cookie="", body=None, peer_container=None):
    url = urllib.parse.urlsplit(origin)
    connection = http.client.HTTPConnection(url.hostname, url.port, timeout=10)
    headers = {"Host": url.netloc, "Origin": origin}
    if path == "/login":
        headers["Accept"] = "text/html"
    if cookie:
        headers["Cookie"] = cookie
    if body is not None:
        headers["Content-Type"] = "application/json"
        body = json.dumps(body)
    connection.request(method, path, body=body, headers=headers)
    response = connection.getresponse()
    payload = response.read()
    if peer_container is not None:
        require_non_loopback_peer(peer_container)
    cookies = [value.split(";", 1)[0] for key, value in response.getheaders() if key.lower() == "set-cookie"]
    connection.close()
    return response.status, payload, cookies


def mcp(origin, path, headers, message, peer_container=None):
    """One MCP POST as an MCP client sends it: no Origin and no cookie unless `headers` add them.
    Returns the status, the issued mcp-session-id, and the JSON-RPC answer or error body."""
    url = urllib.parse.urlsplit(origin)
    connection = http.client.HTTPConnection(url.hostname, url.port, timeout=10)
    connection.request("POST", path, body=json.dumps(message), headers={
        "Host": url.netloc,
        "Content-Type": "application/json",
        "Accept": "application/json, text/event-stream",
        **headers,
    })
    response = connection.getresponse()
    if peer_container is not None:
        require_non_loopback_peer(peer_container)
    session = response.getheader("mcp-session-id")
    answer = None
    if (response.getheader("content-type") or "").startswith("text/event-stream"):
        # rmcp may keep the stream open after its answer, so read up to the first JSON event.
        while answer is None:
            line = response.fp.readline()
            if not line:
                break
            data = line.decode().removeprefix("data:").strip() if line.startswith(b"data:") else ""
            if data:
                answer = json.loads(data)
    elif response.status != 202:
        answer = json.loads(response.read() or b"null")
    connection.close()
    return response.status, session, answer


def check_mcp(name, origin, cookies):
    """GRE-1459: MCP on the explicit listener admits a valid API key from a non-loopback
    peer and refuses no key or a wrong key without creating a session."""
    status, body, _ = request(origin, "POST", "/settings/api-keys", cookies, {})
    if status != 201:
        raise SystemExit(f"creating an API key answered {status}")
    bearer = {"Authorization": f"Bearer {json.loads(body)['key']}"}
    initialize = {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18", "capabilities": {},
        "clientInfo": {"name": "docker-smoke", "version": "0"}}}
    for path in ["/mcp", "/api/v1/mcp"]:
        status, session, answer = mcp(origin, path, bearer, initialize, peer_container=name)
        if status != 200 or not session or "serverInfo" not in (answer or {}).get("result", {}):
            raise SystemExit(f"{path} initialize with a key answered {status}: {answer}")
        headers = {**bearer, "Mcp-Session-Id": session, "Mcp-Protocol-Version": "2025-06-18"}
        status, _, _ = mcp(origin, path, headers, {"jsonrpc": "2.0", "method": "notifications/initialized"})
        if status != 202:
            raise SystemExit(f"{path} initialized notification answered {status}")
        status, _, answer = mcp(origin, path, headers, {"jsonrpc": "2.0", "id": 2, "method": "tools/list"})
        tools = [tool["name"] for tool in ((answer or {}).get("result") or {}).get("tools", [])]
        if status != 200 or "list_plans" not in tools:
            raise SystemExit(f"{path} tools/list answered {status} with {len(tools)} tools")
        call = {"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "list_plans", "arguments": {}}}
        status, _, answer = mcp(origin, path, headers, call)
        result = (answer or {}).get("result") or {}
        if status != 200 or result.get("isError") or not result.get("content"):
            raise SystemExit(f"{path} list_plans answered {status}: {answer}")
        wrong = {"Authorization": "Bearer ppk_0000000000000000000000000000000000000000"}
        for label, headers, expected in [
            ("no key", {"Origin": origin}, 401),
            ("no key, no Origin", {}, 403),
            ("wrong key", wrong, 401),
        ]:
            status, session, _ = mcp(origin, path, headers, initialize)
            if status != expected or session:
                raise SystemExit(f"{path} {label} answered {status} (session {bool(session)}), expected {expected}")
    log(f"MCP over the published port: a key lists {len(tools)} tools and runs list_plans; "
        "no key or a wrong key gets 401 (403 without Origin) and no session")


def require_non_loopback_peer(name):
    # The HTTP connection stays open while we inspect its container-side peer.
    table = docker("exec", name, "cat", "/proc/net/tcp").stdout
    for row in table.splitlines()[1:]:
        fields = row.split()
        if fields[1].endswith(":1F90") and fields[3] == "01":  # port 8080, ESTABLISHED
            peer = ipaddress.IPv4Address(bytes.fromhex(fields[2].split(":")[0])[::-1])
            if not peer.is_loopback and not peer.is_unspecified:
                log("container TCP table confirms a non-loopback peer on port 8080")
                return
    raise SystemExit("published-port request did not arrive from a non-loopback peer")


def inspect_image(image):
    config = json.loads(docker("image", "inspect", image).stdout)[0]["Config"]
    if config["User"] != "1000:1000":
        raise SystemExit(f"image user is {config['User']!r}, expected 1000:1000")
    if "PP_DESKTOP_REBUILD=1" not in config["Env"]:
        raise SystemExit("image does not set PP_DESKTOP_REBUILD=1")
    if "PP_LISTEN_ADDR=0.0.0.0:8080" not in config["Env"]:
        raise SystemExit("image does not set PP_LISTEN_ADDR=0.0.0.0:8080")
    log(f"image user {config['User']}, PP_DESKTOP_REBUILD=1, healthcheck {config['Healthcheck']['Test']}")
    found = docker(
        "run", "--rm", "--entrypoint", "/bin/sh", image, "-c",
        "for tool in node nodejs npm npx python python3 corepack; do command -v $tool; done; "
        "find / -xdev \\( -name node -o -name 'python3*' -o -name npm \\) \\( -type f -o -type l \\) 2>/dev/null; "
        "id -u; id -g",
    ).stdout.split()
    if found != ["1000", "1000"]:
        raise SystemExit(f"runtime is not Node- and Python-free or not uid 1000: {found}")
    log("runtime has no node, npm, npx or python; runs as uid 1000 gid 1000")
    help_text = docker("run", "--rm", image, "pp-server", "healthcheck", "--help").stdout
    if not help_text.startswith("Usage: pp-server healthcheck"):
        raise SystemExit(f"unexpected healthcheck help: {help_text!r}")
    log("pp-server healthcheck --help exits 0")


def start(image, name, volume):
    docker(
        "run", "-d", "--name", name, "-p", "127.0.0.1::8080",
        "--read-only", "--tmpfs", "/tmp:size=64m,mode=1777",
        "--cap-drop", "ALL", "--security-opt", "no-new-privileges",
        "--health-interval", "2s", "--health-start-period", "60s",
        "-v", f"{volume}:/data", image,
        "/usr/local/bin/pp-server-start", "--test-credential-file", CREDENTIAL,
    )
    deadline = time.monotonic() + 120
    while time.monotonic() < deadline:
        result = docker("exec", name, "pp-server", "healthcheck", "readiness", "--status-file", STATUS, check=False)
        if result.returncode == 0:
            break
        state = docker("inspect", "--format", "{{.State.Running}}", name).stdout.strip()
        if state != "true":
            raise SystemExit("container exited during startup:\n" + docker("logs", name, check=False).stderr)
        time.sleep(0.5)
    else:
        raise SystemExit("readiness never passed within 120 s")
    for probe in ["liveness", "readiness"]:
        line = docker("exec", name, "pp-server", "healthcheck", probe, "--status-file", STATUS).stdout.strip()
        log(f"healthcheck {probe}: {line}")
    credential = json.loads(docker("exec", name, "cat", CREDENTIAL).stdout)
    if credential["child_pid"] is not None:
        raise SystemExit("a Node child started")
    binding = json.loads(docker("inspect", name).stdout)[0]["NetworkSettings"]["Ports"]["8080/tcp"][0]
    origin = f"http://127.0.0.1:{binding['HostPort']}"
    if "Listening on 0.0.0.0:8080" not in docker("logs", name).stderr:
        raise SystemExit("container did not report binding PP_LISTEN_ADDR")
    log("docker run: bound 0.0.0.0:8080, reached through published loopback port")
    status, _, _ = request(origin, "GET", "/api/v1/plans", peer_container=name)
    if status != 401:
        raise SystemExit(f"anonymous plans answered {status}, expected 401")
    if not origin.startswith("http://127.0.0.1:"):
        raise SystemExit(f"unexpected origin {origin!r}")
    # Sign in as a headless operator would: the URL comes from docker exec, not
    # from the test-only credential file.
    url = docker("exec", name, "pp-server", "bootstrap-url", "--data", "/data").stdout.strip()
    bootstrap = urllib.parse.urlsplit(url)
    # bootstrap-url knows the container port, not Docker's published host port.
    if f"{bootstrap.scheme}://{bootstrap.netloc}" != "http://127.0.0.1:8080" or bootstrap.path != "/__desktop/bootstrap":
        raise SystemExit("bootstrap-url printed an unexpected URL")
    status, _, cookies = request(origin, "GET", f"{bootstrap.path}?{bootstrap.query}")
    if status != 303 or not cookies:
        raise SystemExit(f"bootstrap answered {status}")
    status, _, _ = request(origin, "GET", f"{bootstrap.path}?{bootstrap.query}")
    if status != 401:
        raise SystemExit(f"a used bootstrap URL answered {status}")
    logs = docker("logs", name)
    token = urllib.parse.parse_qs(bootstrap.query)["token"][0]
    if token in logs.stdout or token in logs.stderr or "__desktop/bootstrap" in logs.stdout + logs.stderr:
        raise SystemExit("the bootstrap URL reached the container logs")
    refused = docker("exec", "-u", "0", name, "pp-server", "bootstrap-url", "--data", "/data", check=False)
    if refused.returncode == 0 or refused.stdout:
        raise SystemExit("root, which does not own /data, got a bootstrap URL")
    log("bootstrap-url: signed in once, a reused URL is refused, no URL in the logs, root is refused")
    desktop = cookies[0]
    for path in ["/api/v1/plans", "/api/v1/integrations/missing/status"]:
        status, _, _ = request(origin, "GET", path, desktop)
        if status != 401:
            raise SystemExit(f"{path} without account login answered {status}, expected 401")
    for path in ["/mcp", "/api/v1/mcp"]:
        # The desktop cookie is not an MCP key; the key guard answers.
        status, _, _ = request(origin, "POST", path, desktop, {})
        if status != 401:
            raise SystemExit(f"MCP without a key answered {status}, expected 401")
    status, page, _ = request(origin, "GET", "/login", desktop)
    if status != 200 or b"<html" not in page.lower():
        raise SystemExit(f"sign-in page through published port answered {status}")
    log("non-loopback peer: login required, MCP without key refused, sign-in page 200")
    return origin, desktop


def wait_health(name):
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        health = docker("inspect", "--format", "{{.State.Health.Status}}", name).stdout.strip()
        if health == "healthy":
            log("docker reports the container healthy")
            return
        time.sleep(1)
    raise SystemExit(f"docker health is {health!r}")


def stop(name):
    processes = docker("top", name, "-eo", "pid,comm").stdout.split("\n")[1:]
    commands = sorted(line.split()[1] for line in processes if line.strip())
    if commands != ["pp-server", "tini"]:
        raise SystemExit(f"unexpected processes before stop: {commands}")
    log(f"processes before stop: {commands}")
    started = time.monotonic()
    docker("stop", "--time", "10", name)
    elapsed = time.monotonic() - started
    state = json.loads(docker("inspect", name).stdout)[0]["State"]
    if state["ExitCode"] != 0 or state["OOMKilled"]:
        raise SystemExit(f"container exited {state['ExitCode']} after SIGTERM")
    receipt = json.loads(docker("logs", name).stdout.strip().splitlines()[-1])
    if receipt.get("storage_released") is not True or receipt.get("errors") != []:
        raise SystemExit(f"incomplete shutdown receipt: {receipt}")
    log(f"SIGTERM: exit 0 in {elapsed:.1f} s, storage_released true, no errors; container has no processes left")


def main():
    if len(sys.argv) != 2:
        raise SystemExit("usage: docker-rust-smoke.py IMAGE")
    image = sys.argv[1]
    suffix = uuid.uuid4().hex[:8]
    volume = f"pp-smoke-data-{suffix}"
    names = [f"pp-smoke-a-{suffix}", f"pp-smoke-b-{suffix}"]
    docker("volume", "create", volume)
    try:
        inspect_image(image)
        account = {"email": "smoke@example.test", "password": "smoke-password-1234"}

        origin, desktop = start(image, names[0], volume)
        status, body, _ = request(origin, "GET", "/health", desktop)
        if status != 200 or b'"modules"' not in body:
            raise SystemExit(f"/health answered {status}")
        status, body, cookies = request(origin, "POST", "/auth/register", desktop, account)
        if status != 200:
            raise SystemExit(f"register answered {status}: {body[:200]!r}")
        status, body, _ = request(origin, "GET", "/api/v1/plans", f"{desktop}; {cookies[0]}")
        if status != 200:
            raise SystemExit(f"plans answered {status}")
        status, _, _ = request(origin, "GET", "/api/v1/integrations/missing/status", f"{desktop}; {cookies[0]}")
        if status != 404:
            raise SystemExit(f"missing-printer status answered {status}, expected 404 after login")
        log("first start: /health 200, account registered, /api/v1/plans 200, missing-printer status 404")
        check_mcp(names[0], origin, f"{desktop}; {cookies[0]}")
        listing = docker("exec", names[0], "ls", "/data").stdout.split()
        log(f"/data holds: {' '.join(sorted(listing))}")
        second = docker("exec", names[0], "/usr/local/bin/pp-server-start", check=False)
        if second.returncode == 0 or "Status file belongs to a running server" not in second.stderr:
            raise SystemExit(f"a second pp-server was not refused: {second.returncode} {second.stderr.strip()}")
        docker("exec", names[0], "pp-server", "healthcheck", "readiness", "--status-file", STATUS)
        log("a second pp-server in the container is refused and the live status file stays")
        wait_health(names[0])
        stop(names[0])

        origin, desktop = start(image, names[1], volume)
        status, body, _ = request(origin, "POST", "/auth/register", desktop, account)
        if status == 200:
            raise SystemExit("register succeeded twice: the account did not persist")
        status, body, cookies = request(origin, "POST", "/auth/login", desktop, account)
        if status != 200 or not cookies:
            raise SystemExit(f"login after restart answered {status}")
        status, _, _ = request(origin, "GET", "/api/v1/plans", f"{desktop}; {cookies[0]}")
        if status != 200:
            raise SystemExit(f"plans after restart answered {status}")
        log("second start on the same volume: login with the stored account 200, /api/v1/plans 200")
        stop(names[1])
        log("PASS")
    finally:
        for name in names:
            docker("rm", "-f", name, check=False)
        docker("volume", "rm", "-f", volume, check=False)


if __name__ == "__main__":
    main()
