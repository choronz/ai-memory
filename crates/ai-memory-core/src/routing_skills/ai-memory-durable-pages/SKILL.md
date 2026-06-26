---
name: ai-memory-durable-pages
description: Use this skill for any explicit durable wiki mutation in ai-memory: saving project knowledge, recording a rule or annotation, updating a permanent note, or deleting a memory page. Trigger by semantic intent rather than exact wording; routine session capture is not a durable-page request.
---
<!-- ai-memory-managed: routing-skill -->

# ai-memory durable pages

Use this skill only for deliberate durable wiki mutations. Routine session capture is automatic, and permanent notes require an explicit user request.

## Tools in this cluster

- `memory_write_page` writes a durable wiki page for permanent project knowledge.
- `memory_delete_page` removes a durable wiki page by exact path.

## Writing durable memory

Write a page only when the user explicitly asks to remember something permanently, save a note, add an annotation, or record project knowledge. Do not use durable pages for transient progress, normal status updates, or next-session context.

Put the page title as a `# H1` on the first line of the body and omit the separate title argument. ai-memory derives the title from that H1. Keep the content concise and fact-like, with enough context that a future agent can apply it without rereading the whole session.

## Project rules belong in instructions first

If the user asks to create a durable project rule such as always do X or never do Y, update the project's canonical agent instruction file when the repository says one exists. Use a durable page only when the user explicitly wants the rule in the wiki too, or when no canonical instruction file applies.

## Deleting durable memory

Delete only by exact path. If the user gives a vague title or topic, first resolve it to the page path using read-only lookup. Preserve sibling projects unless the user explicitly names them.

## Scope default

Default to the current project. Pass workspace and project together only when the user explicitly names a different project. Never pass scope arguments for phrases like this project, here, we, or our work.
