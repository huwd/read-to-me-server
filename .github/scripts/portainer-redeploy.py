#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Redeploy the owner's Portainer stack with freshly pushed images (ADR-0015
Amendment 1).

The stack's definition lives in the owner's infrastructure repo and in Portainer,
not here: this pulls each image on the host, then re-applies the stack's own stored
compose file and environment, so containers whose image changed are recreated.

Images are pulled from 127.0.0.1:5000, the registry as the host itself sees it:
loopback is an insecure-registry address by default, so no daemon change is needed.

Environment:
  PORTAINER_URL          e.g. http://gismo:9000 (reached over the tailnet)
  PORTAINER_ACCESS_KEY   a Portainer API key
  PORTAINER_STACK_ID     the stack to redeploy
  PORTAINER_ENDPOINT_ID  the Docker endpoint the stack runs on
  REGISTRY_USERNAME, REGISTRY_PASSWORD
  IMAGES                 space-separated repository names, e.g. "read-to-me-rtm"
  TAG                    the tag to pull, e.g. latest
"""

import base64
import json
import os
import sys
import urllib.parse
import urllib.request

REGISTRY = "127.0.0.1:5000"


def env(name):
    value = os.environ.get(name, "").strip()
    if not value:
        sys.exit(f"{name} is not set")
    return value


def request(method, url, key, body=None, headers=None, timeout=300):
    data = None if body is None else json.dumps(body).encode()
    all_headers = {"X-API-Key": key}
    if body is not None:
        all_headers["Content-Type"] = "application/json"
    all_headers.update(headers or {})
    req = urllib.request.Request(url, data=data, headers=all_headers, method=method)
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        return resp.read()


def pull(base, key, endpoint, image, tag):
    # Docker decodes X-Registry-Auth as base64url, not standard base64.
    auth = base64.urlsafe_b64encode(
        json.dumps(
            {
                "username": env("REGISTRY_USERNAME"),
                "password": env("REGISTRY_PASSWORD"),
                "serveraddress": REGISTRY,
            }
        ).encode()
    ).decode()
    query = urllib.parse.urlencode({"fromImage": f"{REGISTRY}/{image}", "tag": tag})
    body = request(
        "POST",
        f"{base}/api/endpoints/{endpoint}/docker/images/create?{query}",
        key,
        headers={"X-Registry-Auth": auth},
        timeout=900,
    )
    # Docker answers 200 and reports failures in its progress stream.
    for line in body.decode(errors="replace").splitlines():
        try:
            event = json.loads(line)
        except ValueError:
            continue
        if "error" in event:
            sys.exit(f"pulling {image}:{tag} failed: {event['error']}")
    print(f"pulled {REGISTRY}/{image}:{tag}")


def main():
    base = env("PORTAINER_URL").rstrip("/")
    key = env("PORTAINER_ACCESS_KEY")
    stack_id = int(env("PORTAINER_STACK_ID"))
    endpoint = int(env("PORTAINER_ENDPOINT_ID"))
    tag = env("TAG")

    for image in env("IMAGES").split():
        pull(base, key, endpoint, image, tag)

    stack = json.loads(request("GET", f"{base}/api/stacks/{stack_id}", key))
    stack_file = json.loads(request("GET", f"{base}/api/stacks/{stack_id}/file", key))
    # The images are already on the host; pulling again here would go through the
    # stack file's own image references, which is what the step above just did.
    result = json.loads(
        request(
            "PUT",
            f"{base}/api/stacks/{stack_id}?endpointId={endpoint}",
            key,
            body={
                "stackFileContent": stack_file["StackFileContent"],
                "env": stack.get("Env") or [],
                "prune": False,
                "pullImage": False,
            },
            timeout=600,
        )
    )
    print(f"redeployed stack {result.get('Name', stack_id)} (status {result.get('Status')})")


if __name__ == "__main__":
    main()
