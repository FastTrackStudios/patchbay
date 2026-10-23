#!/usr/bin/env bash
# Sandbox test of scripts/release.sh (flow v4): CalVer next versions, the
# changelog from cargo-rail change files, the release guard, tagging, a
# hotfix line with its forward-port and the GitHub Release, end to end
# on a synthetic repo and a local bare "origin". Nothing leaves the
# sandbox: cargo and gh are shims (gh keeps its releases and PRs as files).
# Run from anywhere:
#
#   scripts/release-sandbox.sh [SCRATCH_DIR]
#
# Needs git and awk; git-cliff (or nix) only for the no-change-files
# fallback. Exits non-zero if any check fails.
# shellcheck disable=SC2015,SC2016 # `[ test ] && ok || bad` is the idiom here: ok never fails; the ERE patterns hold literal backticks
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
S="${1:-${TMPDIR:-/tmp}/release-sandbox}"
REPO=FastTrackStudios/patchbay
export GIT_AUTHOR_NAME=test GIT_AUTHOR_EMAIL=t@example.invalid GIT_COMMITTER_NAME=test GIT_COMMITTER_EMAIL=t@example.invalid
export GIT_CONFIG_GLOBAL=/dev/null
rm -rf "$S"; mkdir -p "$S/shim" "$S/releases" "$S/prs"
export PATH="$S/shim:$PATH"
export GITHUB_REPOSITORY="$REPO"
unset GITHUB_STEP_SUMMARY GITHUB_OUTPUT GH_TOKEN RELEASE_MONTH
pass=0; failn=0
ok()  { echo "PASS: $*"; pass=$((pass+1)); }
bad() { echo "FAIL: $*"; failn=$((failn+1)); }
expect_ok()   { local d="$1"; shift; if "$@" >"$S/last.out" 2>&1; then ok "$d"; else bad "$d"; cat "$S/last.out"; fi; }
expect_fail() { local d="$1"; shift; if "$@" >"$S/last.out" 2>&1; then bad "$d (unexpected pass)"; cat "$S/last.out"; else ok "$d"; grep -h '::error::\|release:' "$S/last.out" | head -2 | sed 's/^/      /'; fi; }
expect_rc()   { local d="$1" want="$2"; shift 2; "$@" >"$S/last.out" 2>&1; local rc=$?; [ "$rc" = "$want" ] && ok "$d (exit $rc)" || { bad "$d (exit $rc, wanted $want)"; cat "$S/last.out"; }; }
expect_out()  { if grep -qiE "$3" "$2"; then ok "$1"; else bad "$1 (no match for: $3)"; sed 's/^/      | /' "$2" | head -30; fi; }
expect_no()   { if grep -qiE "$3" "$2"; then bad "$1 (found: $3)"; else ok "$1"; fi; }
eq()          { [ "$2" = "$3" ] && ok "$1: $2" || bad "$1: got '$2', wanted '$3'"; }
lastpr()      { local f n=0; for f in "$S"/prs/*.meta; do [ -e "$f" ] && n=$((n+1)); done; echo "$n"; } # the newest PR's number

# ── shims ────────────────────────────────────────────────────────────────
# cargo: `update --workspace` moves the lockfile entries of the members that
# inherit the workspace version (as cargo does), nothing else.
cat > "$S/shim/cargo" <<'EOF'
#!/usr/bin/env bash
echo "[shim] cargo $*" >&2
[ "$1" = update ] || exit 0
v="$(sed -n '/^\[workspace\.package\]/,/^\[/{s/^version *= *"\([^"]*\)".*/\1/p;}' Cargo.toml | head -1)"
awk -v v="$v" -v members="patchbay fts-patchbay" '
  BEGIN { n = split(members, m, " "); for (i = 1; i <= n; i++) mem[m[i]] = 1 }
  /^name = / { name = $3; gsub(/"/, "", name) }
  /^version = / && (name in mem) && !unused { print "version = \"" v "\""; next }
  /^\[\[patch\.unused\]\]/ { unused = 1 }
  { print }' Cargo.lock > Cargo.lock.new && mv Cargo.lock.new Cargo.lock
EOF
cat > "$S/shim/gh" <<'EOF'
#!/usr/bin/env bash
# A tiny gh: releases and PRs are files, calls are logged to $SHIM_LOG.
echo "[shim] gh $*" >> "$SHIM_LOG"
arg() { local k="$1"; shift; while [ $# -gt 0 ]; do [ "$1" = "$k" ] && { echo "$2"; return; }; shift; done; }
case "$1 $2" in
  "release view") [ -f "$SHIM_REL/$3.body" ] || exit 1
     case "$*" in *"--json body"*) b="$(cat "$SHIM_REL/$3.body")"; sleep "${SHIM_VIEW_DELAY:-0}"; printf '%s\n' "$b" ;; esac ;;
  "release create") tag="$3"; shift 3
     cp "$(arg --notes-file "$@")" "$SHIM_REL/$tag.body"; arg --title "$@" > "$SHIM_REL/$tag.title" ;;
  "release edit") tag="$3"; shift 3; cp "$(arg --notes-file "$@")" "$SHIM_REL/$tag.body" ;;
  "pr list") exit 0 ;;
  "pr create") shift 2; n=$(( $(ls "$SHIM_PRS" | grep -c meta) + 1 ))
     { echo "base=$(arg --base "$@")"; echo "head=$(arg --head "$@")"; echo "title=$(arg --title "$@")"; } > "$SHIM_PRS/$n.meta"
     f="$(arg --body-file "$@")"; if [ -n "$f" ]; then cp "$f" "$SHIM_PRS/$n.body"; else arg --body "$@" > "$SHIM_PRS/$n.body"; fi
     echo "https://github.com/x/y/pull/$n" ;;
  "api -X") case "$*" in *check-runs*) echo "${SHIM_CHECK:-completed success}" ;; esac ;;
  "api --paginate") : ;;
esac
EOF
chmod +x "$S/shim/cargo" "$S/shim/gh"
export SHIM_LOG="$S/gh.log" SHIM_REL="$S/releases" SHIM_PRS="$S/prs"; : > "$SHIM_LOG"

# ── a synthetic repo: the real script, the real cliff.toml ───────────────
W="$S/seed"; mkdir -p "$W/scripts" "$W/crates/patchbay" "$W/apps/patchbay" "$W/crates/shared" "$W/.changes"
cp "$HERE/scripts/release.sh" "$W/scripts/"; cp "$HERE/cliff.toml" "$W/" 2>/dev/null || true
printf 'Change files: cargo rail change add <crates> --bump <none|patch|minor|major> -m "<note>"\n' > "$W/.changes/README"
printf '[workspace]\nmembers = ["crates/patchbay", "apps/patchbay", "crates/shared"]\n\n[workspace.package]\nversion = "0.1.0"\nedition = "2021"\n' > "$W/Cargo.toml"
printf '[package]\nname = "patchbay"\nversion.workspace = true\n' > "$W/crates/patchbay/Cargo.toml"
printf '[package]\nname = "fts-patchbay"\nversion.workspace = true\n' > "$W/apps/patchbay/Cargo.toml"
printf '[package]\nname = "shared"\nversion = "0.1.0"\n' > "$W/crates/shared/Cargo.toml"
cat > "$W/Cargo.lock" <<'EOF'
# This file is automatically @generated by Cargo.
version = 4

[[package]]
name = "serde"
version = "0.1.2"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "fts-patchbay"
version = "0.1.0"

[[package]]
name = "patchbay"
version = "0.1.0"

[[package]]
name = "shared"
version = "0.1.0"

[[patch.unused]]
name = "a-patch"
version = "1.0.0"

[[patch.unused]]
name = "b-patch"
version = "2.0.0"
EOF
printf '# Changelog\n\nAll notable changes to this project are documented in this file.\n\n' > "$W/CHANGELOG.md"
git -C "$W" init -q -b main; git -C "$W" add -A; git -C "$W" commit -qm "initial import"
# v0.1.0 .. v0.1.2: the pre-CalVer history (flow v2 releases)
for v in 0.1.0 0.1.1 0.1.2; do
  if [ "$v" != 0.1.0 ]; then
    sed -i "s/^version = \"0\.1\.[0-9]\"$/version = \"$v\"/" "$W/Cargo.toml"
    sed -i -E "/^name = \"(fts-)?patchbay\"/{n;s/^version = .*/version = \"$v\"/}" "$W/Cargo.lock"
  fi
  awk -v v="$v" 'done || !/^## \[/ { print; next } { print "## [" v "] - 2026-10-21\n\n- v2 release " v "\n"; print; done = 1 } END { if (!done) print "## [" v "] - 2026-10-21\n\n- v2 release " v }' "$W/CHANGELOG.md" > "$W/cl" && mv "$W/cl" "$W/CHANGELOG.md"
  git -C "$W" commit -qam "chore(release): v$v (#$((${v##*.} + 1)))"; git -C "$W" tag -a "v$v" -m "patchbay $v"
done
git -C "$W" tag v99.1.0-rc.1 -m "a suffixed tag never counts"
git init -q --bare -b main "$S/origin.git"
git -C "$W" push -q "$S/origin.git" main:main --tags
git clone -q "$S/origin.git" "$S/wc"; cd "$S/wc" || exit 1
R() { bash scripts/release.sh "$@"; }
# guard <head sha> [base ref] [head ref]
guard() { BASE_REF="${2:-main}" HEAD_REF="${3:-release/pending}" HEAD_SHA="$1" GITHUB_STEP_SUMMARY="$S/summary.md" bash scripts/release.sh guard; }
release_only() { BASE_REF="${2:-main}" HEAD_REF="${3:-release/pending}" HEAD_SHA="$1" GITHUB_STEP_SUMMARY="$S/summary.md" bash scripts/release.sh release-only; }
# land <subject> <file> [<change file name> <front matter lines> <note>]: a squash-merged PR on <line> (default main)
land() {
  local subj="$1" f="$2" line="${LINE:-main}"
  git fetch -q origin; git switch -q --detach "origin/$line"
  mkdir -p "$(dirname "$f")"; echo "// $subj" >> "$f"; git add "$f"
  if [ $# -ge 5 ]; then printf -- '---\n%b---\n\n%b\n' "$4" "$5" > ".changes/$3.md"; git add ".changes/$3.md"; fi
  git commit -qm "$subj"; git push -q origin "HEAD:$line"; }
# merge_release <line> <pr number>: the release PR squash-merged, the branch deleted
merge_release() {
  local v; git fetch -q origin; v="$(git show origin/release/pending:Cargo.toml | sed -n '/^\[workspace\.package\]/,/^\[/{s/^version *= *"\([^"]*\)".*/\1/p;}')"
  git switch -q --detach "origin/$1"; git merge -q --squash origin/release/pending
  git commit -qm "chore(release): v$v (#$2)"; git push -q origin "HEAD:$1"; git push -q origin --delete release/pending
  git branch -qD release/pending 2>/dev/null; git fetch -q --prune origin; }

echo "=== CalVer: the next version"
export RELEASE_MONTH=26.10
eq "tags v0.1.1, v0.1.2 (and v99.1.0-rc.1) in October 2026" "$(R next 2>/dev/null)" 26.10.0
eq "the same in November" "$(RELEASE_MONTH=26.11 R next 2>/dev/null)" 26.11.0
eq "this month, from the real clock" "$(env -u RELEASE_MONTH bash scripts/release.sh next 2>/dev/null)" "$(date -u +%y).$((10#$(date -u +%m))).0"
expect_fail "RELEASE_MONTH must be YY.M" env RELEASE_MONTH=26.01 bash scripts/release.sh next

echo "=== change files -> CHANGELOG (first v4 release: 0.1.2 -> 26.10.0)"
land "feat(signal): offline drafts (#70)" crates/shared/a.rs added-drafts '"patchbay" = "minor"\n"shared" = "patch"\n' "Added offline drafts (signal)"
land "fix(session): reconnect backoff (#71)" crates/shared/b.rs fixed-backoff '"fts-patchbay" = "patch"\n' "Fixed reconnect backoff resetting after sleep (session)"
land "feat(shared)!: rename sync.peer (#72)" crates/shared/c.rs renamed-peer '"shared" = "major"\n' "Renamed config key \`sync.peer\` to \`sync.peers\` (session)\n\nMigration: run \`session migrate-config\`; no database migration."
land "ci: faster plan (#73)" .github/x.yml ci-plan '"shared" = "none"\n' "CI plans faster"
git switch -q --detach origin/main
expect_ok "plan" R plan; R plan > "$S/plan.out" 2>&1
expect_out "plan: next is 26.10.0" "$S/plan.out" 'next release: *26\.10\.0'
expect_out "plan: four pending change files" "$S/plan.out" 'pending change files: *4'
expect_out "plan: the draft has the Added group" "$S/plan.out" '^### Added'
expect_fail "prepare 26.10.5: not the next version" R prepare 26.10.5
expect_fail "prepare 26.01.0: not CalVer" R prepare 26.01.0
expect_ok "prepare 26.10.0" R prepare 26.10.0
eq "branch" "$(git rev-parse --abbrev-ref HEAD)" release/pending
eq "workspace version" "$(R version)" 26.10.0
eq "subject" "$(git log -1 --format=%s)" "chore(release): v26.10.0"
eq "one commit on main" "$(git rev-list --count origin/main..HEAD)" 1
eq "change files consumed (only the README is left)" "$(git ls-tree --name-only HEAD .changes/ | paste -sd' ' -)" .changes/README
R notes 26.10.0 > "$S/n.md"; cp "$S/n.md" "$S/section-26.10.0.md"
expect_out "Breaking / Migration holds the major note" "$S/n.md" '^### Breaking / Migration'
expect_out "  ...with its Migration paragraph indented under it" "$S/n.md" '^  Migration: run `session migrate-config`'
expect_out "Added holds the minor note" "$S/n.md" '^- Added offline drafts \(signal\)$'
expect_out "Fixed and improved holds the patch note" "$S/n.md" '^- Fixed reconnect backoff resetting after sleep \(session\)$'
expect_no  "the none note is skipped" "$S/n.md" 'CI plans faster'
[ "$(grep -n '^### ' "$S/n.md" | cut -d: -f2- | paste -sd'|' -)" = "### Breaking / Migration|### Added|### Fixed and improved" ] \
  && ok "groups in order: Breaking, Added, Fixed" || bad "group order: $(grep '^### ' "$S/n.md" | paste -sd'|' -)"
grep -n '^## \[' CHANGELOG.md | head -2 > "$S/heads"; expect_out "the new section sits above [0.1.2]" "$S/heads" '^[0-9]+:## \[26\.10\.0\].*$'
git diff HEAD^ HEAD -- Cargo.lock > "$S/lock.diff"
expect_out "Cargo.lock: patchbay moved" "$S/lock.diff" '^\+version = "26\.10\.0"'
eq "Cargo.lock: shared (own version) did not move" "$(grep -A1 'name = "shared"' Cargo.lock | tail -1)" 'version = "0.1.0"'
expect_ok "release-pr" R pr
expect_out "PR: into main" "$S/prs/1.meta" '^base=main$'
expect_out "PR: title is the release commit" "$S/prs/1.meta" '^title=chore\(release\): v26\.10\.0$'
expect_out "PR body: template sections" "$S/prs/1.body" '^## Change files$'
expect_out "PR body: the changelog" "$S/prs/1.body" 'Added offline drafts'
REL=$(git rev-parse HEAD)

echo "=== the release guard"
expect_ok "guard passes" guard "$REL"
expect_ok "an ordinary PR is not checked" guard "$REL" main feat/sta-1-thing
grep -q 'not a release PR' "$S/last.out" && ok "  ...and says so" || bad "  ...message"
expect_ok "a fork's release/pending is not a release PR" env HEAD_REPO=someone/patchbay BASE_REF=main HEAD_REF=release/pending HEAD_SHA="$REL" bash scripts/release.sh guard
expect_rc "release-only: a sound release PR" 0 release_only "$REL"
expect_rc "release-only: an ordinary PR" 1 release_only "$REL" main feat/sta-1-thing
git switch -q -c t/two "$REL"; echo x > extra.txt; git add extra.txt; git commit -qm "fix: sneaked in"; TWO=$(git rev-parse HEAD)
expect_fail "guard: a two-commit release branch" guard "$TWO"
expect_rc "release-only: that one fails the required check" 3 release_only "$TWO"
git switch -q --detach "$REL"; git switch -q -c t/file; echo x > extra.txt; git add extra.txt; git commit -q --amend --no-edit; FILE=$(git rev-parse HEAD)
expect_fail "guard: one commit that also changes a non-version file" guard "$FILE"
grep -q 'extra.txt: not a release file' "$S/last.out" && ok "  ...naming the file" || bad "  ...message"
git switch -q --detach "$REL"; git switch -q -c t/toml; sed -i 's/^edition = "2021"/edition = "2024"/' Cargo.toml; git commit -qa --amend --no-edit
expect_fail "guard: Cargo.toml changes more than the version" guard "$(git rev-parse HEAD)"
git switch -q --detach "$REL"; git switch -q -c t/lock; sed -i '/name = "serde"/{n;s/.*/version = "26.10.0"/}' Cargo.lock; git commit -qa --amend --no-edit
expect_fail "guard: a non-member (registry) crate's version moved in Cargo.lock" guard "$(git rev-parse HEAD)"
grep -q 'non-member packages changed version: "serde"' "$S/last.out" && ok "  ...naming serde" || bad "  ...message"
# reorder the [[patch.unused]] trailer (as cargo does at random): still sound
git switch -q --detach "$REL"; git switch -q -c t/unused
awk '/^\[\[patch\.unused\]\]/ { u = 1 } !u { print } u { t = t $0 "\n" } END { n = split(t, b, "\n\n"); printf "%s\n\n%s", b[2], b[1] "\n" }' Cargo.lock > l && mv l Cargo.lock
git commit -qa --amend --no-edit; tail -7 Cargo.lock > "$S/unused.out"
expect_out "  (the trailer is reordered)" "$S/unused.out" '^name = "b-patch"'
expect_ok "guard: a reordered [[patch.unused]] trailer is not a change" guard "$(git rev-parse HEAD)"
git switch -q --detach "$REL"; git switch -q -c t/old; sed -i 's/^- v2 release 0\.1\.1$/- rewritten/' CHANGELOG.md; git commit -qa --amend --no-edit
expect_fail "guard: a released changelog section edited" guard "$(git rev-parse HEAD)"
git switch -q --detach "$REL"; git switch -q -c t/ver; sed -i 's/26\.10\.0/26.10.3/g' Cargo.toml Cargo.lock CHANGELOG.md; git commit -qa --amend -m "chore(release): v26.10.3"
expect_fail "guard: not the next version (26.10.3)" guard "$(git rev-parse HEAD)"
grep -q 'is not the next version 26.10.0' "$S/last.out" && ok "  ...naming the next version" || bad "  ...message"
git switch -q --detach "$REL"; git switch -q -c t/subj; git commit -q --amend -m "chore: release"
expect_fail "guard: wrong subject" guard "$(git rev-parse HEAD)"
git switch -q --detach "$REL"
expect_fail "guard: unreachable origin fails, never passes" env BASE_REF=main HEAD_REF=release/pending HEAD_SHA="$REL" GIT_DIR="$S/wc/.git" bash -c "git remote set-url origin $S/nowhere.git; bash scripts/release.sh guard; rc=\$?; git remote set-url origin $S/origin.git; exit \$rc"
land "docs: a late PR (#74)" docs/late.md
git switch -q --detach "$REL"
expect_fail "guard: main moved since the cut (stale release PR)" guard "$REL"
grep -q 'not on top of the CURRENT main' "$S/last.out" && ok "  ...saying main moved" || bad "  ...message"
expect_fail "prepare: refuses while release/pending is on origin" R prepare
git push -q origin --delete release/pending; git branch -qD release/pending
git switch -q --detach origin/main
expect_ok "re-cut after main moved" R prepare; git push -q origin release/pending; REL=$(git rev-parse HEAD)
expect_ok "guard passes on the re-cut" guard "$REL"

echo "=== merge (squash) and tag"
merge_release main 80; M1=$(git rev-parse origin/main)
SHIM_CHECK="completed failure" expect_fail "tag: refused while Build + test is red" R tag
grep -q "is 'completed failure', not green" "$S/last.out" && ok "  ...naming the check result" || bad "  ...message"
SHIM_CHECK="in_progress null" expect_fail "tag: refused while Build + test is running" R tag
expect_ok "tag (Build + test green)" R tag
git fetch -q --tags origin
eq "v26.10.0 is on the squash merge commit" "$(git rev-parse 'v26.10.0^{commit}')" "$M1"
eq "annotated" "$(git cat-file -t v26.10.0)" tag
expect_fail "tag again: never moved" R tag 26.10.0
git switch -q --detach v26.10.0
expect_ok "tag-guard on v26.10.0" env GITHUB_REF=refs/tags/v26.10.0 bash scripts/release.sh tag-guard
expect_ok "tag-guard with GH_TOKEN checks Build + test" env GITHUB_REF=refs/tags/v26.10.0 GH_TOKEN=x bash scripts/release.sh tag-guard
SHIM_CHECK="completed failure" expect_fail "tag-guard with GH_TOKEN: red Build + test" env GITHUB_REF=refs/tags/v26.10.0 GH_TOKEN=x bash scripts/release.sh tag-guard
expect_fail "tag-guard: a branch ref" env GITHUB_REF=refs/heads/main bash scripts/release.sh tag-guard
# a tag whose versions and changelog are right, on a commit no release line has
git switch -q -c t/stray v26.10.0; sed -i 's/26\.10\.0/26.10.9/g' Cargo.toml CHANGELOG.md; git commit -qam "chore(release): v26.10.9"; git tag v26.10.9
expect_fail "tag-guard: a commit on neither main nor release/v*" env GITHUB_REF=refs/tags/v26.10.9 bash scripts/release.sh tag-guard
grep -q 'is on neither main nor a release/v\* line' "$S/last.out" && ok "  ...for that reason" || bad "  ...refused for another reason"
git tag -d v26.10.9 >/dev/null; git switch -q --detach v26.10.0

echo "=== publish"
expect_ok "publish v26.10.0" env GITHUB_REF=refs/tags/v26.10.0 bash scripts/release.sh publish
expect_out "release notes: the changelog section" "$S/releases/v26.10.0.body" 'Added offline drafts'
expect_out "release notes: on main" "$S/releases/v26.10.0.body" 'on main'
expect_out "release title" "$S/releases/v26.10.0.title" '^patchbay 26\.10\.0$'
expect_ok "publish again is a no-op" env GITHUB_REF=refs/tags/v26.10.0 bash scripts/release.sh publish

echo "=== the next release in the same month: 26.10.1, then 26.10.2"
land "fix(signal): tuner jitter (#81)" crates/shared/d.rs fixed-tuner '"patchbay" = "patch"\n' "Fixed tuner needle jitter (signal)"
git switch -q --detach origin/main
expect_ok "prepare" R prepare; eq "version" "$(R version)" 26.10.1
git push -q origin release/pending; expect_ok "guard" guard "$(git rev-parse HEAD)"
merge_release main 82; expect_ok "tag v26.10.1" R tag
git fetch -q --tags origin
eq "next after v26.10.0, v26.10.1 in October" "$(R next 2>/dev/null)" 26.10.2

echo "=== hotfix on release/v26.10.1, forward-port into main"
land "feat(session): half-done mixer (#83)" crates/shared/e.rs added-mixer '"fts-patchbay" = "minor"\n' "Added the new mixer (session)"
land "fix(signal): crash on preset load (#84)" crates/shared/f.rs fixed-crash '"patchbay" = "patch"\n' "Fixed a crash when loading a preset (signal)"
FIX=$(git rev-parse origin/main)
land "feat(session): more unshippable work (#85)" crates/shared/g.rs added-more '"fts-patchbay" = "minor"\n' "Added more (session)"
git switch -q --detach origin/main
expect_fail "hotfix: a commit not on main is refused" R hotfix "$(git rev-parse t/stray)"
expect_ok "hotfix $FIX" R hotfix "$FIX"
HB="$(git rev-parse --abbrev-ref HEAD)"
eq "hotfix branch name" "$HB" "fix/no-issue-hotfix-$(git rev-parse --short "$FIX")"
eq "release/v26.10.1 created at the tag" "$(git rev-parse origin/release/v26.10.1)" "$(git rev-parse 'v26.10.1^{commit}')"
git log -1 --format=%B > "$S/pick.out"; expect_out "cherry-picked with -x" "$S/pick.out" "cherry picked from commit $FIX"
P=$(lastpr); expect_out "hotfix PR into release/v26.10.1" "$S/prs/$P.meta" '^base=release/v26\.10\.1$'
expect_out "hotfix PR title is the fix's" "$S/prs/$P.meta" '^title=fix\(signal\): crash on preset load$'
# the hotfix PR squash-merged into the release line
git switch -q --detach origin/release/v26.10.1; git merge -q --squash "$HB"; git commit -qm "fix(signal): crash on preset load (#86)"; git push -q origin HEAD:release/v26.10.1
expect_ok "prepare --hotfix" R prepare --hotfix
eq "hotfix version (next free number)" "$(R version)" 26.10.2
R notes 26.10.2 > "$S/n.md"
expect_out "hotfix section: the fix" "$S/n.md" 'Fixed a crash when loading a preset'
expect_no  "hotfix section: nothing unshipped from main" "$S/n.md" 'mixer|Added more'
expect_ok "release-pr (hotfix)" R pr
P=$(lastpr); expect_out "release PR into the hotfix line" "$S/prs/$P.meta" '^base=release/v26\.10\.1$'
HREL=$(git rev-parse HEAD)
expect_ok "guard: release PR into release/v26.10.1" guard "$HREL" release/v26.10.1
expect_fail "guard: the same commit is no release into main" guard "$HREL" main
merge_release release/v26.10.1 87
expect_ok "tag v26.10.2 on the hotfix line" R tag
git fetch -q --tags origin
expect_out "  ...it says so" "$S/last.out" 'on release/v26\.10\.1'
git merge-base --is-ancestor v26.10.2 origin/main && bad "v26.10.2 is not on main" || ok "v26.10.2 is on the release line, not main"
P=$(lastpr); expect_out "forward-port PR into main" "$S/prs/$P.meta" '^base=main$'
FP="$(sed -n 's/^head=//p' "$S/prs/$P.meta")"
eq "forward-port branch" "$FP" chore/no-issue-forward-port-v26-10-2
git fetch -q origin
git show "origin/$FP:CHANGELOG.md" | grep -n '^## \[' | head -3 | cut -d: -f2- | paste -sd'|' - > "$S/fp.out"
expect_out "forward-port: [26.10.2] above [26.10.1] on main" "$S/fp.out" '^## \[26\.10\.2\].*\|## \[26\.10\.1\]'
eq "forward-port: the consumed change file is deleted on main" "$(git ls-tree --name-only "origin/$FP" .changes/ | grep -c fixed-crash)" 0
eq "forward-port: the unshipped change files stay" "$(git ls-tree --name-only "origin/$FP" .changes/ | grep -c 'added-mixer\|added-more')" 2
expect_ok "tag-guard on v26.10.2 (release/v* line)" env GITHUB_REF=refs/tags/v26.10.2 bash -c 'git switch -q --detach v26.10.2 && bash scripts/release.sh tag-guard'
# the forward-port merges; main's next release
git switch -q --detach origin/main; git merge -q --squash "origin/$FP"; git commit -qm "chore(release): forward-port the v26.10.2 changelog (#88)"; git push -q origin HEAD:main
git switch -q --detach origin/main
expect_ok "prepare on main after the hotfix" R prepare
eq "version" "$(R version)" 26.10.3
R notes 26.10.3 > "$S/n.md"
expect_out "main's section: includes the hotfix" "$S/n.md" '^Includes all changes up to v26\.10\.2\.$'
expect_out "main's section: the mixer" "$S/n.md" 'Added the new mixer'
expect_no  "main's section: the hotfixed crash is not listed twice" "$S/n.md" 'Fixed a crash'
eq "next month, after v26.10.x" "$(RELEASE_MONTH=26.11 R next 2>/dev/null)" 26.11.0
git switch -q --detach origin/main; git branch -qD release/pending

echo "=== no change files at all: git-cliff over the range (pre-v4 history)"
if [ -f cliff.toml ] && { command -v git-cliff >/dev/null || command -v nix >/dev/null; }; then
  # main rewound to v26.10.1 (no pending change files) plus one commit
  # without a change file: only possible in the sandbox
  git switch -q --detach v26.10.1
  echo "// x" >> crates/shared/z.rs; git add -A; git commit -qm "fix(signal): a v2-era commit (#90)"
  git push -q -f origin HEAD:main
  expect_ok "prepare falls back to git-cliff" R prepare
  R notes "$(R version)" > "$S/n.md"
  expect_out "fallback section lists the commit" "$S/n.md" 'v2-era commit'
else
  echo "SKIP: no git-cliff / nix for the fallback"
fi

echo; echo "=== RESULT: $pass passed, $failn failed"
[ "$failn" = 0 ]
