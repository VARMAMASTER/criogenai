# Criogen, LangGraph and CrewAI: the comparison kit

This is the exact benchmark behind the numbers on
https://criogenai.com/benchmarks/ . Run it on your own machine and
compare your results with ours.

What it does: the same agents, workflows, loads and injected faults run on
Criogen, LangGraph and CrewAI, all in Python, with one simulated model
(no network, no API keys). Each framework gets its own virtual environment,
and every measurement runs in a fresh process. `METHOD.md` describes every
scenario and test; the code for each framework is in `frameworks/`.

## Run it

Needs Python 3.10 to 3.13 and about 1.5 GB of free disk for the three
virtual environments (CrewAI alone is about 750 MB).

```sh
python setup.py      # once: a virtual environment per framework (pinned versions)
python run.py        # everything, about 25 minutes; writes bench/results/<date>-<machine>.json
python run.py quick  # a short smoke test (not for comparing)
```

Then open https://criogenai.com/benchmarks/ and drop the results
file into "Check it on your machine": your numbers appear next to ours.
Nothing is uploaded; the page reads the file in your browser.

If `pip` fails because of a private or broken package index in your pip
configuration, run `python setup.py --pip-isolated`.

## Licence

The kit is free to use, copy and change, to reproduce or extend the
comparison.
