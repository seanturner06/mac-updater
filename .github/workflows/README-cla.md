# Setting up the CLA check

One-time setup for [`cla.yml`](cla.yml). Everything below happens on GitHub, not locally.

## 1. Commit the CLA and pin its version

Signatures must reference an immutable copy of the agreement. A link to `main` is not good
enough — if the text later changes, nobody can prove what they actually agreed to.

```sh
git add .github/CLA.md .github/workflows/cla.yml
git commit -m "Add contributor licence agreement and CLA check"
git push

# Get the SHA of the commit that contains CLA.md
git log -1 --format=%H -- .github/CLA.md
```

Paste that SHA into the `path-to-document` line of `cla.yml`, replacing `<COMMIT-SHA>`, then
commit again. Verify the resulting URL loads before relying on it.

**If you ever change the CLA text:** bump `path-to-signatures` to `signatures/v2/cla.json` and
point `path-to-document` at the new commit. Everyone re-signs. Editing the text while leaving
the path alone means existing signatures silently refer to a document that no longer exists.

## 2. Create the signatures branch

The action writes to an orphan branch so signature history stays out of `main`.

```sh
git checkout --orphan cla-signatures
git rm -rf .
mkdir -p signatures/v1
echo '{"signedContributors":[]}' > signatures/v1/cla.json
git add signatures/v1/cla.json
git commit -m "Initialise CLA signature store"
git push -u origin cla-signatures
git checkout main
```

The action can create this itself on the first signature, but doing it explicitly means the
first contributor's experience isn't the thing that discovers a permissions problem.

## 3. Allow Actions to write to the repository

**Settings → Actions → General → Workflow permissions** → select **Read and write
permissions**, then Save.

Without this, `GITHUB_TOKEN` is read-only and the action fails when it tries to record a
signature.

## 4. Make the check required

**Settings → Branches → Add branch ruleset** (or Add rule) for `main`:

- Require a pull request before merging
- Require status checks to pass → add **CLA Assistant** once it has run at least once
  (GitHub only lists checks it has seen before, so open a throwaway PR from a second account or
  a fork to make it appear)

This is what actually enforces the agreement. Without it the bot comments and nothing stops a
merge.

## 5. Test it

Open a pull request from an account that is **not** in the `allowlist` — a second GitHub
account or a fork. Expected behaviour:

1. The bot comments asking for a signature, and the CLA status check fails.
2. Replying with exactly `I have read the CLA Document and I hereby sign the CLA` flips the
   check to passing within a minute or so.
3. `signatures/v1/cla.json` on the `cla-signatures` branch gains an entry with the username,
   user ID, PR number, and timestamp.

Step 3 is the one to actually verify — that file is the legal record. It is worth backing up
somewhere outside GitHub.

## Keeping it current

`contributor-assistant/github-action` is pinned to a version in `cla.yml`. Check the
[action's README](https://github.com/contributor-assistant/github-action) for the current
release and whether any input names have changed before bumping it — the input list has
changed between major versions.

## Alternative: the hosted service

[cla-assistant.io](https://cla-assistant.io) is the hosted version — sign in with GitHub,
authorise it, link the repo, and point it at a **GitHub Gist** containing the CLA text. Less
setup, nicer UI, no workflow file.

It was not chosen here because the signature records live on someone else's infrastructure.
That record is the thing that makes the agreement enforceable, and it may need to be produced
years from now. Owning it is worth a workflow file.
