#!/usr/bin/env python3
"""Re-run the release workflow's own step bodies, outside GitHub Actions.

The `run:` payloads are read out of `.github/workflows/release.yml` (PyYAML)
rather than re-typed, so this checks the workflow text. The one step that runs
`gh release` (`Publish`) is never executed.

    python3 scripts/verify-release-workflow.py all
    python3 scripts/verify-release-workflow.py linux    # prepare, linux, checksums, notes
    python3 scripts/verify-release-workflow.py guards   # the dry run cannot publish
    python3 scripts/verify-release-workflow.py mac      # the macos job under stubs

`linux` runs the prepare and linux jobs with the GITHUB_* environment of a `v*`
tag push, then the checksum and notes bodies on the `dist/` it left (removed
again unless --keep); it needs a release build. `mac` replaces `lipo`,
`pkgbuild`, `pkgutil` and `productbuild` with recording stubs: it checks how the
scripts call those tools, not the tools themselves.

Needs PyYAML. Exit status: 0 all requested checks passed, 1 a check failed,
2 environment error.
"""
import argparse
import os
import re
import shutil
import subprocess
import sys
import tempfile
import textwrap
from pathlib import Path

try:
    import yaml
except ModuleNotFoundError:
    sys.exit("verify-release-workflow: needs PyYAML (pip install pyyaml)")

REPO = Path(__file__).resolve().parents[1]
WORKFLOW = REPO / ".github" / "workflows" / "release.yml"
BINARIES = ("ff", "fflocated", "ff-gui")
EXPR = re.compile(r"\$\{\{\s*([^}]+?)\s*\}\}")

failures = []


def check(what, ok, detail=""):
    print(("  ok:   " if ok else "  FAIL: ") + what + (f"  [{detail}]" if detail and not ok else ""))
    if not ok:
        failures.append(what)
    return ok


def workspace_version():
    block = (REPO / "Cargo.toml").read_text().split("[workspace.package]", 1)[1]
    for line in block.splitlines():
        m = re.match(r'\s*version\s*=\s*"([^"]+)"', line)
        if m:
            return m.group(1)
    raise SystemExit("verify-release-workflow: no [workspace.package] version")


TAG = "v" + workspace_version()


def load_workflow():
    wf = yaml.safe_load(WORKFLOW.read_text())
    return wf, (wf.get(True) or wf.get("on"))  # PyYAML reads `on:` as True


def steps_of(wf, job):
    """(name, script, env) for every `run:` step of a job, in order."""
    return [(s.get("name", "<unnamed>"), s["run"], s.get("env") or {})
            for s in wf["jobs"][job]["steps"] if "run" in s]


def resolve_env(raw_env, known, where):
    """Replace `${{ ... }}` in a step's env with values known locally."""
    def sub(match):
        expr = match.group(1)
        if expr not in known:
            raise SystemExit(f"verify-release-workflow: {where}: cannot resolve ${{{{ {expr} }}}}")
        return str(known[expr])
    return {key: EXPR.sub(sub, str(value)) for key, value in raw_env.items()}


def run_step(label, script, env, cwd):
    print(f"\n=== {label} ===")
    proc = subprocess.run(
        ["bash", "--noprofile", "--norc", "-e", "-o", "pipefail", "-c", script],
        cwd=cwd, env=env, text=True, capture_output=True,
    )
    sys.stdout.write(proc.stdout)
    sys.stderr.write(proc.stderr)
    print(f"  exit {proc.returncode}")
    return proc.returncode


def github_env(**overrides):
    """The GITHUB_* environment of a `v*` tag push, unless overridden."""
    env = dict(os.environ)
    env.update(GITHUB_EVENT_NAME="push", GITHUB_REF=f"refs/tags/{TAG}",
               GITHUB_REF_NAME=TAG, GITHUB_REF_TYPE="tag")
    env.update(overrides)
    return env


def run_job(wf, job, base_env, known, cwd):
    """Run a job's steps in order; returns (ok, outputs)."""
    outputs = {}
    for name, script, raw_env in steps_of(wf, job):
        outfile = Path(tempfile.mkstemp(prefix="gh-output-")[1])
        env = dict(base_env, GITHUB_OUTPUT=str(outfile),
                   **resolve_env(raw_env, known, f"{job}/{name}"))
        rc = run_step(f"{job}: {name}", script, env, cwd)
        outputs.update(line.partition("=")[::2] for line in outfile.read_text().splitlines())
        outfile.unlink()
        if rc != 0:
            return check(f"{job}/{name} exit 0", False, f"exit {rc}"), outputs
    return True, outputs


def mode_linux(wf, triggers):
    print("## linux: the prepare and linux jobs, as written in the YAML")
    check("workflow is triggered by v* tags",
          (triggers.get("push") or {}).get("tags") == ["v*"])
    base = github_env()
    ok, outputs = run_job(wf, "prepare", base, {}, REPO)
    if not ok:
        return False
    version = outputs.get("version", "")
    ok = check("prepare resolved the workspace version", version == workspace_version(), version)
    ok = run_job(wf, "linux", base, {"needs.prepare.outputs.version": version}, REPO)[0] and ok

    print("\n## publish: the checksum and notes bodies, over what the linux job built")
    print("  [skipped] Publish -- the only step that runs `gh release`")
    dist = REPO / "dist"
    artifacts = [p for p in sorted(dist.glob("*")) if p.is_file()]
    for name, script, _ in steps_of(wf, "publish"):
        if name in ("Checksums", "Release notes"):
            ok = run_step(f"publish: {name}", script, base, REPO) == 0 and ok

    sums = dist / "SHA256SUMS"
    text = sums.read_text() if sums.is_file() else ""
    listed = [line.split("  ")[-1] for line in text.splitlines()]
    check("SHA256SUMS lists each artifact, and not itself",
          sorted(listed) == [p.name for p in artifacts], text)
    notes = REPO / "notes.md"
    text = notes.read_text() if notes.is_file() else ""
    check("the release notes name each artifact",
          all(p.name in text for p in artifacts),
          str([p.name for p in artifacts if p.name not in text]))
    return ok


def mode_guards(wf, triggers):
    print("## guards: the dry run cannot reach publishing")
    check("workflow_dispatch exists", "workflow_dispatch" in triggers)
    pub = wf["jobs"]["publish"]
    check("publish is gated on a v* tag push",
          pub.get("if") == "github.event_name == 'push' && startsWith(github.ref, 'refs/tags/v')",
          str(pub.get("if")))
    check("workflow default permissions are read-only", wf.get("permissions") == {"contents": "read"})
    check("only publish may write", pub.get("permissions") == {"contents": "write"})
    mentions = [job for job, body in wf["jobs"].items() if "gh release" in str(body)]
    check("`gh release` appears only in publish", mentions == ["publish"], str(mentions))
    # `gh` resolves owner/name from the git remote, and publish has no checkout
    # (it only downloads artifacts), so the repo has to arrive in the
    # environment — the first tag run failed exactly there.
    publish_env = next((s.get("env") or {} for s in pub["steps"]
                        if s.get("name") == "Publish"), {})
    check("publish tells gh which repo to use",
          publish_env.get("GH_REPO") == "${{ github.repository }}", str(publish_env))
    check("no publish step checks out the repository",
          not any("actions/checkout" in str(s.get("uses", "")) for s in pub["steps"]))

    guards = [s["run"] for s in pub["steps"] if s.get("name", "").startswith("Refuse")]
    if not check("the publish job has one ref guard step", len(guards) == 1, str(len(guards))):
        return False

    def guard_case(label, want, **env):
        proc = subprocess.run(
            ["bash", "--noprofile", "--norc", "-e", "-o", "pipefail", "-c", guards[0]],
            env=github_env(**env), text=True, capture_output=True)
        print(f"  {label}: exit {proc.returncode}  "
              f"{(proc.stdout + proc.stderr).strip().splitlines()[:1]}")
        return check(f"guard: {label} -> exit {want}", proc.returncode == want, str(proc.returncode))

    ok = guard_case("dispatch on main", 1, GITHUB_EVENT_NAME="workflow_dispatch",
                    GITHUB_REF="refs/heads/main", GITHUB_REF_NAME="main",
                    GITHUB_REF_TYPE="branch")
    ok = guard_case("branch push", 1, GITHUB_REF="refs/heads/main",
                    GITHUB_REF_NAME="main", GITHUB_REF_TYPE="branch") and ok
    ok = guard_case("tag push", 0) and ok
    return ok


# Each stub records its arguments and produces what the next tool reads; what
# the arguments must be is asserted from that log, so the fixtures stay small.
STUBS = {
    "lipo": """
        #!/bin/sh
        echo "lipo $*" >> "$STUB_LOG"
        case "$1" in
          -archs) echo "${STUB_ARCHS:-x86_64 arm64}" ;;
          -create)
            out= prev=
            for a in "$@"; do [ "$prev" = -output ] && out=$a; prev=$a; done
            [ -n "$out" ] || exit 1
            : > "$out"
            chmod 0755 "$out" ;;
          *) exit 1 ;;
        esac
        """,
    "pkgbuild": """
        #!/bin/sh
        echo "pkgbuild $*" >> "$STUB_LOG"
        root=
        while [ $# -gt 1 ]; do [ "$1" = --root ] && root=$2; shift; done
        [ -d "$root" ] || exit 1
        # `! -type d`, not `-type f`: /usr/local/bin/ff-gui is a symlink into the
        # app bundle, and a payload listing that drops it is the check failing.
        # The `./` prefixes are the real `pkgutil --payload-files` shape, so the
        # packaging script's normalisation is exercised rather than assumed.
        ( cd "$root" && find . ! -type d ) > "$1"
        cp "$1" "${STUB_LOG}.payload"
        """,
    "pkgutil": """
        #!/bin/sh
        cat "$2"
        """,
    "productbuild": """
        #!/bin/sh
        echo "productbuild $*" >> "$STUB_LOG"
        pkg=
        while [ $# -gt 1 ]; do [ "$1" = --package ] && pkg=$2; shift; done
        [ -f "$pkg" ] || exit 1
        cp "$pkg" "$1"
        """,
}


def mode_mac(wf, triggers):
    print("## mac: the macos job under recording stubs")
    work = Path(tempfile.mkdtemp(prefix="verify-rel-mac-"))
    stubs = work / "stubs"
    stubs.mkdir()
    for name, body in STUBS.items():
        (stubs / name).write_text(textwrap.dedent(body))
        (stubs / name).chmod(0o755)

    # Deliberately 0644: a merged binary that lost its exec bit is still
    # packageable, because the packaging script installs it 0755.
    for triple in ("aarch64-apple-darwin", "x86_64-apple-darwin"):
        release = work / "target" / triple / "release"
        release.mkdir(parents=True)
        for binary in BINARIES:
            (release / binary).write_text("fake\n")

    # The workflow checks the repo out at its cwd; the steps call scripts/ by
    # relative path, so the scratch tree needs one too.
    (work / "scripts").symlink_to(REPO / "scripts")

    log = work / "stub.log"
    base = dict(os.environ, PATH=f"{stubs}:{os.environ['PATH']}", STUB_LOG=str(log))
    steps = {name: (script, env) for name, script, env in steps_of(wf, "macos")}
    version = workspace_version()
    ok = True

    script, _ = steps["Universal binaries"]
    ok = run_step("macos: Universal binaries", script, dict(base), work) == 0 and ok
    ok = check("three merged binaries written",
               all((work / "universal" / b).is_file() for b in BINARIES)) and ok

    script, raw_env = steps["Package"]
    env = dict(base, **resolve_env(raw_env, {"needs.prepare.outputs.version": version},
                                   "macos/Package"))
    ok = run_step("macos: Package", script, env, work) == 0 and ok
    ok = check("the .pkg is produced",
               (work / "dist" / f"fflocate-{version}-macos-universal.pkg").is_file()) and ok
    recorded = log.read_text() if log.exists() else ""
    print("  recorded tool invocations:")
    for line in recorded.splitlines():
        print("    " + line)
    ok = check("pkgbuild got the identifier and install-location right",
               "--identifier com.fflocate.pkg" in recorded
               and "--install-location /" in recorded) and ok

    # The payload is what the installer puts on the disk: the GUI has to arrive
    # as a launchable bundle in /Applications, with the CLI names still on PATH.
    payload_file = Path(str(log) + ".payload")
    files = [line[2:] if line.startswith("./") else line
             for line in payload_file.read_text().splitlines()] if payload_file.is_file() else []
    ok = check("the .app bundle is in the payload",
               "Applications/fflocate.app/Contents/Info.plist" in files
               and "Applications/fflocate.app/Contents/MacOS/ff-gui" in files,
               str(files)) and ok
    ok = check("ff-gui is on PATH as a link into the bundle",
               "usr/local/bin/ff-gui" in files) and ok
    ok = check("the CLI and daemon are installed",
               {"usr/local/bin/ff", "usr/local/bin/fflocated"} <= set(files)) and ok

    # Negatives, straight at the packaging script: there is no Mac here to run
    # the real tools, and both preconditions must stop it before productbuild.
    # The bundle script is a precondition of its own: no ff-gui is refused
    # there too, with nothing packaged.
    rc = run_step("macos-app-bundle.sh: no ff-gui",
                  f"scripts/macos-app-bundle.sh {version} '{work}/empty' '{work}/Bundles/fflocate.app'",
                  dict(base), work)
    ok = check("a bundle without ff-gui is refused", rc != 0, f"exit {rc}") and ok

    one_arch = work / "one-arch"
    short = work / "short"
    one_arch.mkdir()
    short.mkdir()
    for binary in BINARIES:
        shutil.copy(work / "universal" / binary, one_arch / binary)
        if binary != "ff-gui":
            shutil.copy(work / "universal" / binary, short / binary)

    log.unlink(missing_ok=True)
    rc = run_step("package-macos-pkg.sh: one-architecture input",
                  f"scripts/package-macos-pkg.sh {version} '{one_arch}' '{work}/dist2'",
                  dict(base, STUB_ARCHS="arm64"), work)
    ok = check("a one-architecture input is refused", rc != 0, f"exit {rc}") and ok
    ok = check("productbuild never ran",
               "productbuild" not in (log.read_text() if log.exists() else "")) and ok

    rc = run_step("package-macos-pkg.sh: no ff-gui",
                  f"scripts/package-macos-pkg.sh {version} '{short}' '{work}/dist3'",
                  dict(base), work)
    ok = check("a missing binary is refused", rc != 0, f"exit {rc}") and ok

    shutil.rmtree(work, ignore_errors=True)
    return ok


MODES = {"linux": mode_linux, "guards": mode_guards, "mac": mode_mac}


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("mode", nargs="?", default="all", choices=["all", *MODES])
    ap.add_argument("--keep", action="store_true", help="leave dist/ and notes.md")
    args = ap.parse_args()

    if not WORKFLOW.is_file():
        sys.exit(f"verify-release-workflow: no {WORKFLOW}")
    wf, triggers = load_workflow()
    dist_existed = (REPO / "dist").exists()

    ok = True
    for name, mode in MODES.items():
        if args.mode in (name, "all"):
            ok = mode(wf, triggers) and ok

    if not (args.keep or dist_existed):
        shutil.rmtree(REPO / "dist", ignore_errors=True)
        (REPO / "notes.md").unlink(missing_ok=True)

    print()
    if failures:
        print(f"FAILED ({len(failures)}): " + "; ".join(failures))
        return 1
    print("all requested checks passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
