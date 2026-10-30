Seeds for the `tool_args` fuzz target: the first byte picks a tool from
`gitcko_tools::NAMES` (one past the end is an unknown tool), the rest is the
argument JSON a model would send.
