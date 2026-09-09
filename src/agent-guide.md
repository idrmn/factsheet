# factsheet - project memory for agents

Store: {project_root}/.factsheet/facts.jsonl (opaque; edit only via factsheet).
Root = nearest dir up from CWD with .factsheet/ or .git; else CWD.
No git? Run the first `factsheet add` from the project root to anchor the store.
One fact = one line, self-contained.

## Read

    factsheet                  # whole store, bare `[id] fact` lines (hook injects this)
    factsheet ls               # full lines with #tags and (date)
    factsheet tags             # unique tags with fact counts
    factsheet ls -t deploy,vm  # facts carrying any of these tags
    factsheet ls --stale 30d   # facts not written for 30+ days
    factsheet ls -t ref        # reference facts excluded from the inject
    factsheet find "query"     # top facts by word overlap (locate a fact to fix)
    factsheet projects         # every project with a store (global, self-pruning)

Reserved tag `ref` (push vs pull). Test: would a fresh agent know to
search for this fact at the moment it matters? yes -> ref; no -> keep hot.

  ref (pull):  credentials, keys, IPs, host maps, resource names,
               command recipes, API endpoints, descriptive mechanics,
               diagnostic references
  hot (push):  MUST / MUST NOT invariants, traps with silent failures,
               data-loss and security risks, policy

ref facts are not injected; the inject footer shows their tag counts.
Demote: factsheet edit <id> -t ref,<other-tags> . Promote: retag without ref.
When in doubt keep it hot: a wrongly-hot fact wastes tokens, a
wrongly-ref fact repeats the mistake memory exists to stop.

## Line types (never classify, never re-derive)

    [id] value | check: <cmd>   # use value; run <cmd> only if the value
                                # fails, then `factsheet edit <id>` with the fix
    [id] check: <cmd>           # run <cmd>; never store its output

## Write

Consent gate: every write (`add`, `edit`, `drop`) needs explicit user
approval. Propose the exact fact text and tags, wait for a yes. No
approval -> no write.

Trigger: a repeated error or gotcha a fresh session would hit again.
A fact must (a) change a future action AND (b) contradict defaults or docs.
Facts owned elsewhere (docs, tickets, tool help, global instructions) stay
there. No brain-dump. Tags are the only grouping; run `factsheet tags` before
inventing a new one. `factsheet add` prints `similar:` lines when existing
facts overlap the new one - review them: same fact -> `factsheet drop` the new
id and `factsheet edit` the old one instead.

Choose form at write time: will the value survive a week untouched?
- yes -> store value with -c "<verify cmd>"
- no  -> store only the check line (-c without text)

    factsheet add "value that survives a week" -c "verify cmd" -t tag1,tag2
    factsheet add "durable value, no check needed" -t tag
    factsheet add -c "cmd to run each session" -t tag    # pure check line

## Fact style

Hard limit on text length, enforced - too-long facts are rejected
and the error names the current cap. Limits come from the user's
config file; agents treat it as read-only.
One fact = one rule: a finding with three rules is three facts sharing
a topic prefix ("azure db: ...") and tags; commands go to -c. The
prefix and tags carry coherence, not line length. A card too big to
split is a doc - move it out, keep a pointer fact.

One line, readable with zero context: name the exact
tool, path, flag - never "it", "this", "the config". State the rule,
not the story of finding it; no hedging ("sometimes", "seems").
Cut every word that does no work; a verbatim command or number beats
prose about it. Actionable fact = condition -> action ("X fails -> do Y").
Reuse terms already in the store: one name per thing, or duplicates hide.

    bad:  we discovered deploys can sometimes fail due to caching issues
    good: deploy: run `make clean` first; stale asset cache breaks hashes

## Maintain

Spot a wrong or dead fact -> propose the fix or drop to the user;
apply it only after approval. Inject warns that hot facts outgrew the
threshold -> curate with `factsheet dedup`: it lists over-length facts (split
each into atomic ones) and near-duplicate pairs (merge or drop one);
then drop dead facts and demote lookups to ref. Contradicting facts:
keep the one verified now, `factsheet drop` the loser.

    factsheet edit <id> "new text"    # provided fields replace old; text/check
                                # edits refresh ts, tags-only edits keep it
    factsheet edit <id> -c "new cmd"
    factsheet edit <id> -t ""         # clear tags
    factsheet drop <id>
