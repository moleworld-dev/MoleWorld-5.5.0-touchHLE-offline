#!/usr/bin/env python3
"""现签 MoleWorldHD 的 IOS_APP_DEVELOPMENT provisioning profile(覆盖 17PM + 16PM)。

证书按【钥匙串里当前有私钥那张】的 sha1 匹配——Xcode(尤其 beta)会轮换 Apple Development 证书,
所以每次先用 `security find-identity -v -p codesigning` 看当前 sha1;若与 CERT_SHA1 不同,
把新值填进环境变量 MW_CERT_SHA1 或直接改下面的默认值。
依赖:ASC API key 在 ~/.appstoreconnect/private_keys/AuthKey_6N5DAM7RXC.p8;pip 包 cryptography。
输出:/tmp/moleworldhd_dev_fresh.mobileprovision(mw-deploy-17pm.sh 会读这个路径)。
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
DEVICES = ["00008150-001E10A62132401C", "00008140-00122C642413C01C"]  # 17PM, 16PM
PROFILE_NAME = "moleworldhd dev auto"
OUT = "/tmp/moleworldhd_dev_fresh.mobileprovision"


def current_cert_sha1() -> str:
    """优先用 MW_CERT_SHA1;否则自动取钥匙串里第一张有私钥的 Apple Development 证书。"""
    env = os.environ.get("MW_CERT_SHA1")
    if env:
        return env.lower()
    out = subprocess.run(
        ["security", "find-identity", "-v", "-p", "codesigning"],
        capture_output=True, text=True,
    ).stdout
    for line in out.splitlines():
        if "Apple Development" in line:
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

dev_ids = []
for udid in DEVICES:
    d = api("GET", f"/v1/devices?filter[udid]={udid}")["data"]
    if d:
        dev_ids.append(d[0]["id"])
    else:
        nd = api("POST", "/v1/devices", {"data": {"type": "devices", "attributes": {
            "name": "dev-" + udid[-6:], "platform": "IOS", "udid": udid}}})
        dev_ids.append(nd["data"]["id"])
print("devices:", dev_ids)

for p in api("GET", "/v1/profiles?limit=200")["data"]:
    if p["attributes"]["name"] == PROFILE_NAME:
        api("DELETE", f"/v1/profiles/{p['id']}")
        print("deleted old profile", p["id"])

resp = api("POST", "/v1/profiles", {"data": {
    "type": "profiles",
    "attributes": {"name": PROFILE_NAME, "profileType": "IOS_APP_DEVELOPMENT"},
    "relationships": {
        "bundleId": {"data": {"type": "bundleIds", "id": bid}},
        "certificates": {"data": [{"type": "certificates", "id": cert_id}]},
        "devices": {"data": [{"type": "devices", "id": d} for d in dev_ids]},
    }}})
open(OUT, "wb").write(base64.b64decode(resp["data"]["attributes"]["profileContent"]))
print("WROTE", OUT)
