"""Validate the public release channels, or find an already-published crate."""

import argparse
import hashlib
import io
import json
import os
import re
import sys
import tarfile
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen

REPO = "riii111/terraleph"
API = f"https://api.github.com/repos/{REPO}"
TAP = "https://raw.githubusercontent.com/riii111/homebrew-terraleph/main/Formula/terraleph.rb"
INDEX = "https://index.crates.io/te/rr/terraleph"
UNIX = (
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "aarch64-unknown-linux-gnu",
    "x86_64-unknown-linux-gnu",
)
ARCHIVES = tuple(f"terraleph-{target}.tar.gz" for target in UNIX) + (
    "terraleph-x86_64-pc-windows-msvc.zip",
)


def fetch(url):
    headers = {"User-Agent": "Terraleph release verification (https://github.com/" + REPO + ")"}
    if url.startswith(API + "/") and os.environ.get("GH_TOKEN"):
        headers["Authorization"] = "Bearer " + os.environ["GH_TOKEN"]
    with urlopen(Request(url, headers=headers), timeout=15) as response:
        return response.read()


def check(condition, message):
    if not condition:
        raise ValueError(message)


def check_github_and_tap(tag):
    release = json.loads(fetch(f"{API}/releases/tags/{tag}"))
    check(
        release["tag_name"] == tag and not release["draft"] and not release["prerelease"],
        f"GitHub {tag} is not a public stable release",
    )
    assets = {asset["name"]: asset for asset in release["assets"]}
    for name in ARCHIVES:
        for filename in (name, f"{name}.sha256"):
            asset = assets.get(filename)
            check(
                asset and asset["state"] == "uploaded" and asset["size"] > 0,
                f"GitHub release is missing {filename}",
            )
        check(
            re.fullmatch(r"sha256:[0-9a-f]{64}", assets[name]["digest"] or ""),
            f"GitHub release has no SHA-256 digest for {name}",
        )
        digest = assets[name]["digest"].removeprefix("sha256:")
        sidecar_url = f"https://github.com/{REPO}/releases/download/{tag}/{name}.sha256"
        sidecar = fetch(sidecar_url).decode("ascii")
        check(
            re.fullmatch(rf"{digest}[ \t]+\*?{re.escape(name)}\r?\n?", sidecar),
            f"GitHub {name}.sha256 does not match the archive digest and filename",
        )

    formula = fetch(TAP).decode()
    version = tag[1:]
    check(re.search(rf'^  version "{re.escape(version)}"$', formula, re.M),
          f"Homebrew tap does not point to {version}")
    for name in ARCHIVES[:-1]:
        url = f"https://github.com/{REPO}/releases/download/{tag}/{name}"
        digest = assets[name]["digest"].removeprefix("sha256:")
        check(
            re.search(rf'url "{re.escape(url)}"\s+sha256 "{digest}"', formula),
            f"Homebrew tap has the wrong URL or checksum for {name}",
        )


def check_crate(tag, commit):
    version = tag[1:]
    try:
        lines = fetch(INDEX).splitlines()
    except HTTPError as error:
        if error.code == 404:
            return False
        raise
    row = next((row for row in map(json.loads, lines) if row["vers"] == version), None)
    if row is None:
        return False
    check(not row["yanked"], f"crates.io {version} is yanked")
    data = fetch(f"https://static.crates.io/crates/terraleph/terraleph-{version}.crate")
    check(hashlib.sha256(data).hexdigest() == row["cksum"],
          f"crates.io {version} download does not match the index checksum")
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as archive:
        source = archive.extractfile(f"terraleph-{version}/.cargo_vcs_info.json")
        check(source is not None, "crate has no source commit metadata")
        check(json.load(source).get("git", {}).get("sha1") == commit,
              f"crates.io {version} was not packaged from {commit}")
    return True


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("mode", choices=("verify", "github-and-tap", "crate-status"))
    parser.add_argument("tag")
    parser.add_argument("commit")
    args = parser.parse_args()
    try:
        check(re.fullmatch(r"v\d+\.\d+\.\d+", args.tag), "Expected a stable version tag")
        check(re.fullmatch(r"[0-9a-f]{40}", args.commit), "Expected a full commit SHA")
        if args.mode != "crate-status":
            check_github_and_tap(args.tag)
        if args.mode != "github-and-tap":
            exists = check_crate(args.tag, args.commit)
            if args.mode == "crate-status":
                print("present" if exists else "missing")
                return 0
            check(exists, f"crates.io does not list {args.tag}")
        print(f"{args.tag}: verified {args.mode}")
        return 0
    except (ValueError, KeyError, TypeError, URLError, OSError, tarfile.TarError) as error:
        print(f"Release verification failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
