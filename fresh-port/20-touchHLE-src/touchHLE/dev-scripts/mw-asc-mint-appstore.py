#!/usr/bin/env python3
"""现签 MoleWorldHD 的 **IOS_APP_STORE** provisioning profile(发 TestFlight 用)。

与 mw-asc-mint.py(开发侧载那张)的区别:①用 **Apple Distribution** 证书而不是 Apple Development;
②App Store 类型不绑设备(profileType=IOS_APP_STORE,没有 devices 关系);③entitlements 里
get-task-allow=false —— make-testflight.sh 会从这张 profile 里抽 entitlements 做发布签名。
证书同样按【钥匙串里当前有私钥那张】的 sha1 匹配(可用 MW_DIST_SHA1 覆盖)。
依赖:ASC API key 在 ~/.appstoreconnect/private_keys/AuthKey_6N5DAM7RXC.p8;pip 包 cryptography。
输出:/tmp/moleworldhd_appstore.mobileprovision。
"""
import base64
import hashlib
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request

from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import ec, utils

KEY_ID = "6N5DAM7RXC"
ISSUER = "0f1cb134-9497-45fb-959c-09fb3a7cf633"
P8 = os.path.expanduser("~/.appstoreconnect/private_keys/AuthKey_6N5DAM7RXC.p8")
BUNDLE = "org.touchhle.moleworldhd"
PROFILE_NAME = "moleworldhd appstore auto"
OUT = "/tmp/moleworldhd_appstore.mobileprovision"


def current_cert_sha1() -> str:
    """优先用 MW_DIST_SHA1;否则自动取钥匙串里第一张有私钥的 Apple Distribution 证书。"""
    env = os.environ.get("MW_DIST_SHA1")
    if env:
        return env.lower()
    out = subprocess.run(
        ["security", "find-identity", "-v", "-p", "codesigning"],
        capture_output=True, text=True,
    ).stdout
    for line in out.splitlines():
        if "Apple Distribution" in line:
            return line.split()[1].lower()
    sys.exit("钥匙串里没有 Apple Development 证书;先在 Xcode 里登录账号生成一张")


def b64url(b: bytes) -> bytes:
    return base64.urlsafe_b64encode(b).rstrip(b"=")


def make_jwt() -> str:
    key = serialization.load_pem_private_key(open(P8, "rb").read(), password=None)
    now = int(time.time())
    hdr = json.dumps({"alg": "ES256", "kid": KEY_ID, "typ": "JWT"}, separators=(",", ":")).encode()
    pl = json.dumps(
        {"iss": ISSUER, "iat": now, "exp": now + 1000, "aud": "appstoreconnect-v1"},
        separators=(",", ":"),
    ).encode()
    signing = b64url(hdr) + b"." + b64url(pl)
    r, s = utils.decode_dss_signature(key.sign(signing, ec.ECDSA(hashes.SHA256())))
    return (signing + b"." + b64url(r.to_bytes(32, "big") + s.to_bytes(32, "big"))).decode()


TOK = make_jwt()


def api(method: str, path: str, body=None):
    req = urllib.request.Request(
        "https://api.appstoreconnect.apple.com" + path,
        data=json.dumps(body).encode() if body is not None else None,
        method=method,
    )
    req.add_header("Authorization", "Bearer " + TOK)
    req.add_header("Content-Type", "application/json")
    try:
        raw = urllib.request.urlopen(req).read()
        return json.loads(raw) if raw else {}
    except urllib.error.HTTPError as e:
        print("HTTP", method, path, e.code, e.read().decode()[:400])
        raise


cert_sha1 = current_cert_sha1()
print("使用证书 sha1:", cert_sha1)

bid = api("GET", f"/v1/bundleIds?filter[identifier]={BUNDLE}")["data"][0]["id"]
print("bundle:", bid)

cert_id = None
for c in api("GET", "/v1/certificates?limit=200")["data"]:
    der = base64.b64decode(c["attributes"]["certificateContent"])
    if hashlib.sha1(der).hexdigest().lower() == cert_sha1:
        cert_id = c["id"]
        print("cert:", cert_id, c["attributes"].get("certificateType"))
        break
if not cert_id:
    sys.exit("ASC 上找不到该 sha1 的证书(刚生成的证书可能还没同步;或本机证书不是这个账号的)")


for p in api("GET", "/v1/profiles?limit=200")["data"]:
    if p["attributes"]["name"] == PROFILE_NAME:
        api("DELETE", f"/v1/profiles/{p['id']}")
        print("deleted old profile", p["id"])

resp = api("POST", "/v1/profiles", {"data": {
    "type": "profiles",
    "attributes": {"name": PROFILE_NAME, "profileType": "IOS_APP_STORE"},
    "relationships": {
        "bundleId": {"data": {"type": "bundleIds", "id": bid}},
        "certificates": {"data": [{"type": "certificates", "id": cert_id}]},
    }}})
open(OUT, "wb").write(base64.b64decode(resp["data"]["attributes"]["profileContent"]))
print("WROTE", OUT)
