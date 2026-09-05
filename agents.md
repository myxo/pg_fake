Before doing any tasks, read

docs/spec.md - full specification of project
docs/code_style.md

If user ask to continue with a plan, read docs/plan.md and choose first task that not marked as complete.
If there is anything uncomplete in task description, you must ask user question until evry uncertanty is resolve.

After finishing task call subagent to review changes. Do this until review dont find any problem.
After that give brief summary of what you did, give examples of sql queries that now
processed by pg_fake (if applicable) and wait for user approval. Only then mark task as complete.

If user ask for commit, make concise explanatory commit message. If task include new supported sql
features, add query examples to commit message

General rules:
- do not change code if I asked the question without asking for action
- do not run git commands unless I specifically told you to
