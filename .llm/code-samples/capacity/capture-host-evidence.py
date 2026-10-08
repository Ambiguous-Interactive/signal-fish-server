"""Capture operator-declared evidence with the deployment's binary and environment."""

import argparse
import datetime
import hashlib
import json
from pathlib import Path
import socket
import subprocess


def binary_identity(path):
    digest = hashlib.sha256()
    size = 0
    with path.open("rb") as binary:
        while chunk := binary.read(1024 * 1024):
            digest.update(chunk)
            size += len(chunk)
    return digest.hexdigest(), size


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--endpoint", required=True)
    parser.add_argument("--deployment", required=True)
    parser.add_argument("--host", default=socket.gethostname())
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    before = binary_identity(binary)
    validated = subprocess.run(
        [str(binary), "--validate-config"],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if validated.returncode != 0:
        raise ValueError("the host configuration did not pass validation")
    # Preserve the operator's working directory and SIGNAL_FISH environment.
    # A rerun of the loader is evidence about those inputs, not a live-process probe.
    captured = subprocess.run(
        [str(binary), "--print-config-evidence"],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if captured.returncode != 0:
        raise ValueError("the config evidence command failed")
    evidence = json.loads(captured.stdout)
    effective = evidence["effective"]
    security = effective["security"]
    referenced = {"security.app_auth_path": security["app_auth_path"]}
    token = security["connect_token"]
    if token:
        referenced["security.connect_token.public_key_path"] = token["public_key_path"]
    tls = security["transport"]["tls"]
    if tls["enabled"]:
        for field in ("certificate_path", "private_key_path", "client_ca_cert_path"):
            referenced[f"security.transport.tls.{field}"] = tls[field]
    file_hashes = {
        field: binary_identity(Path(path))[0]
        for field, path in referenced.items()
        if path is not None
    }
    if before != binary_identity(binary):
        raise ValueError("the binary changed during evidence collection")
    evidence.update(
        endpoint=args.endpoint,
        host=args.host,
        deployment=args.deployment,
        collected_at_rfc3339=datetime.datetime.now(datetime.timezone.utc).isoformat(),
        binary_sha256=before[0],
        binary_bytes=before[1],
        file_sha256=file_hashes,
    )
    # Refuse to overwrite a previous capture. The runner embeds this document.
    with args.output.open("x", encoding="utf-8") as output:
        json.dump(evidence, output, ensure_ascii=False, indent=2)
        output.write("\n")


if __name__ == "__main__":
    main()
