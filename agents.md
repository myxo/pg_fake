Before doing any tasks, read

docs/spec.md - full specification of project
docs/code_style.md

If user ask to continue with a plan, read docs/plan.md and choose first task that not marked as complete.
If there is anything uncomplete in task description, you must ask user question until evry uncertanty is resolve.

After finishing task call subagent to review changes. Do this until review dont find any problem.

After task is finished, make commit with concise explanatory commit message. If task include new supported sql
features, add query examples to commit message.

General rules:
- do not change code if I asked the question without asking for action
