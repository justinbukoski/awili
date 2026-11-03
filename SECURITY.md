# Security policy

## Reporting a vulnerability

Please report security problems privately, not in a public issue. Use GitHub's
**Report a vulnerability** button on the Security tab of this repository
(private vulnerability reporting). Include what you found, how to reproduce it,
and the Awili version (Awili menu, About).

I will acknowledge a report within a week and aim to ship a fix or a clear
mitigation within 90 days. Please give me that time before disclosing publicly.

## Supported versions

Only the latest release candidate or release receives fixes.

## Scope notes

Awili runs commands and edits files at your request, using agents you install.
The approval prompts and folder confinement are part of its security model, so a
way to bypass them is in scope. Bugs in third-party agents, models or services
you connect to are not.
