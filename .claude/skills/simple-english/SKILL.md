---
name: simple-english
description: Write new user-facing prose in plain, unambiguous English, using the structural rules of ASD-STE100 Simplified Technical English. Use when writing or rewriting a README, a doc under docs/, a runbook, an operational note, help text, or an error message. Also use when asked for "simple english", "plain english", "STE", or text a non-native reader must not misread.
allowed-tools: Read, Edit, Write, Grep, Glob
---

# Simple English

New user-facing prose is read by someone who is tired, in a hurry, and
not a native speaker of English. Each sentence must survive one read.

The structural rules below come from ASD-STE100 Simplified Technical
English, the controlled language that aerospace maintenance manuals use.
The standard is a free download at asd-ste100.org. Its approved-word
dictionary is copyrighted and is not reproduced here, so this doctrine
takes the structure and not the word list. Never claim STE compliance
for our text.

## Scope

This governs **new** prose that a user or an operator reads:

- `README.md` and the files under `docs/`
- runbooks and operational notes
- error messages, help text, and log lines a human is meant to act on
- release notes

It governs nothing else. Three kinds of text keep their own rules:

| Text | Governed by |
|------|-------------|
| Commit messages | `.gitlint` and the `/commit` skill |
| Code, nix, and script comments | the comment rules in `CLAUDE.md` |
| `CLAUDE.md` and other agent instructions | nothing here |

Existing prose is not rewritten to match. Rewrite a paragraph when you
change what it says, not because this doctrine arrived.

## Step 1: classify the passage

Every other rule follows from this.

| | Procedural | Descriptive |
|---|---|---|
| Job | Tell the reader what to do | Explain what a thing is or does |
| Verb | Imperative: "Run the check." | Simple present or simple past |
| Sentence limit | 20 words | 25 words |
| Unit | One instruction per sentence | One topic per paragraph, six sentences at most |

Do not mix the two in one passage. A note inside a procedure is
descriptive text and takes the 25-word limit.

## Step 2: write

**Short and complete.** Short does not mean clipped. Keep the articles
and keep the word "that". Write "Make sure that the file exists before
you run the command", not "Ensure file exists before running".

**One sentence, one new fact.** Give the information gradually.

**Conditions first.** Put the condition before the command, divided by
a comma. "If the build fails, read the log." Never the other way round.

**Active voice.** In descriptive text, passive voice is allowed only
when the actor is unknown.

**Modals: `can`, `will`, `must`.** Do not use `should`, `would`, `may`,
`might`, or `could`. A requirement is `must`. A possibility is `can`. A
recommendation is stated as a fact or deleted. A reader treats "should"
as optional, and so does a model.

**Simple tenses.** Infinitive, imperative, simple present, simple past,
simple future. A past participle is an adjective only, as in "the
cached response". No present perfect, and no "is to be configured".

**Actions are verbs.** "Compress the file", not "perform compression of
the file".

**No `-ing` verbs.** An `-ing` word is allowed only inside a noun, as
in "logging".

**Noun chains stop at three words.** Break a longer one with a
preposition. Write "the timeout value for the connection pool", not
"the connection pool timeout configuration value".

**No semicolons.** Write two sentences.

**No contractions.** Write "do not", not "don't". Write "it is", not
"it's".

**No phrasal verbs.** Write "decrease", not "go down". Write
"configure", not "set up".

**Warnings.** Name the risk level, give the command or the condition,
then give the consequence. The instruction never comes last.

```
CAUTION: Do not run this against the bench. It reflashes every board
in the rack.
```

## Step 3: one word, one meaning

Pick one term for each concept before you draft. Then use no other word
for it anywhere in the document. The rotations that cost the most:

- check, verify, confirm, validate, ensure — pick one
- config, configuration, settings, options — pick one
- run, execute, invoke, launch — pick one
- show, display, render, print — pick one
- "error" for an error, "failure" for an operation that failed

Our domain words are legal words. `flake`, `probe`, `defmt`, `flash`,
`deploy`, and `pin` stay as they are.

## Step 4: delete the slop

| Instead of | Write |
|------------|-------|
| leverage, utilize | use |
| in order to | to |
| prior to | before |
| ensure | make sure that |
| due to the fact that | because |
| in the event that | if |
| when it comes to | for |
| enables you to, allows you to | you can |
| functionality | function, feature |
| facilitate | help |
| out of the box | by default |
| under the hood | internally |
| e.g., i.e. | for example, that is |
| etc. | name the items, or write "and more" |
| and/or | pick one, or write "X, or Y, or both" |
| simply, just, easily, seamlessly | delete |
| robust, powerful, comprehensive | delete, or give the measured value |
| it is worth noting that | delete |
| is designed to, aims to | delete, and say what it does |
| gracefully handles | say what it does instead |

A word that carries no fact is deleted, not replaced.

## Untouchables

Leave these exact, even where they break a rule above. Each counts as
one word against the sentence limit.

- code blocks, inline code, identifiers, commands, flags, paths
- quoted error messages and log lines
- product, chip, and endpoint names
- numbers with units

## Step 5: check before you deliver

This step is not optional.

1. Count the words in the three longest sentences. Split anything over
   the limit for its class.
2. Search the draft for `should`, `would`, `may`, `might`, `could`,
   `has been`, `have been`, a semicolon, and an apostrophe contraction.
   Correct every hit.
3. Search for every `if` and `when`. Each one starts its sentence.
4. Search for the terms you rejected in Step 3. Replace every hit with
   the term you picked.
