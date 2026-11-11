// VibeRoller: ready-to-paste harness setups for Settings → API access (T-369, G4). Pure functions, no DOM: the page
// (web/index.html, "Connect a tool") renders what setups() returns, and the suite drives them in node (R565).
// Never a real key unless the caller passes one it just created; with none the text says <your key>.
(function () {
  const W = typeof window !== 'undefined' ? window : globalThis;
  const NO_KEY = '<your key>', NO_MODEL = '<model id>';

  // the hub's own address: the status gives the OpenAI base (".../v1"); the Anthropic SDK wants it without /v1
  function originOf(endpoint, fallback) {
    return String(endpoint || fallback || '').trim().replace(/\/+$/, '').replace(/\/v1$/, '');
  }
  // the person's chosen model, else the first one the key may use
  function pickModel(chosen, models) {
    if (chosen) return chosen;
    const m = (models || [])[0];
    return m ? (typeof m === 'string' ? m : m.id || '') : '';
  }

  function setups(o) {
    o = o || {};
    const origin = originOf(o.origin) || '<hub address>', key = o.key || NO_KEY, model = o.model || NO_MODEL, base = origin + '/v1';
    const list = [];
    list.push({ id: 'claude-code', title: 'Claude Code', lang: 'shell',
      text: [
        '# Linux / macOS (bash, zsh)',
        `export ANTHROPIC_BASE_URL=${origin}`,
        `export ANTHROPIC_AUTH_TOKEN=${key}`,
        `export ANTHROPIC_MODEL=${model}   # optional: leave out to use the default of your key`,
        'claude',
        '',
        '# Windows (PowerShell)',
        `$env:ANTHROPIC_BASE_URL = "${origin}"`,
        `$env:ANTHROPIC_AUTH_TOKEN = "${key}"`,
        `$env:ANTHROPIC_MODEL = "${model}"   # optional`,
        'claude',
      ].join('\n'),
      note: 'Works on Linux and Windows. ANTHROPIC_MODEL is optional. If this machine has a Claude login, run /logout first so the token is used.' });
    list.push({ id: 'codex', title: 'Codex', lang: 'toml',
      text: [
        '# ~/.codex/config.toml  (Windows: %USERPROFILE%\\.codex\\config.toml)',
        `model = "${model}"`,
        'model_provider = "viberoller"',
        '',
        '[model_providers.viberoller]',
        'name = "VibeRoller"',
        `base_url = "${base}"`,
        'env_key = "VIBEROLLER_API_KEY"',
        'wire_api = "chat"',
        '',
        '# then, in the shell you start codex from:',
        `export VIBEROLLER_API_KEY=${key}                 # bash`,
        `$env:VIBEROLLER_API_KEY = "${key}"               # PowerShell`,
      ].join('\n'),
      note: 'Uses the chat completions API (wire_api = "chat"). The Responses API that Codex prefers comes later.' });
    list.push({ id: 'jcode', title: 'jcode / Jcode PGM', lang: 'shell',
      text: [
        '# bash',
        `printf '%s' '${key}' | jcode provider add viberoller \\`,
        `  --base-url ${base} \\`,
        `  --model ${model} \\`,
        '  --api-key-stdin --set-default',
        '',
        '# PowerShell',
        `"${key}" | jcode provider add viberoller --base-url ${base} --model ${model} --api-key-stdin --set-default`,
        '',
        '# check it',
        'jcode --provider-profile viberoller auth-test --no-tool-smoke',
      ].join('\n'),
      note: 'Adds an OpenAI-compatible profile named viberoller to ~/.jcode/config.toml. Needs a provider with an API key behind your key.' });
    list.push({ id: 'opencode', title: 'opencode', lang: 'json',
      text: JSON.stringify({
        $schema: 'https://opencode.ai/config.json',
        provider: { viberoller: { npm: '@ai-sdk/openai-compatible', name: 'VibeRoller', options: { baseURL: base, apiKey: key }, models: { [model]: { name: model } } } },
        model: 'viberoller/' + model,
      }, null, 2),
      note: 'Save as opencode.json in your project, or ~/.config/opencode/opencode.json for every project.' });
    list.push({ id: 'openai', title: 'Cursor / Continue / any OpenAI client', lang: 'text',
      text: [
        `Base URL (OpenAI API): ${base}`,
        `API key: ${key}`,
        `Model: ${model}`,
        '',
        '# environment form many OpenAI clients read',
        `export OPENAI_BASE_URL=${base}`,
        `export OPENAI_API_KEY=${key}`,
      ].join('\n'),
      note: 'In Cursor: Settings, Models, override the OpenAI base URL, paste the key, add the model id. In Continue: provider "openai" with apiBase and apiKey.' });
    list.push({ id: 'curl-openai', title: 'curl: chat completions', lang: 'shell',
      text: [
        `curl ${base}/chat/completions \\`,
        `  -H "Authorization: Bearer ${key}" \\`,
        '  -H "Content-Type: application/json" \\',
        `  -d '{"model":"${model}","messages":[{"role":"user","content":"Say hello"}]}'`,
      ].join('\n'), note: 'OpenAI style. Add "stream": true to stream.' });
    list.push({ id: 'curl-anthropic', title: 'curl: messages', lang: 'shell',
      text: [
        `curl ${origin}/v1/messages \\`,
        `  -H "x-api-key: ${key}" \\`,
        '  -H "anthropic-version: 2023-06-01" \\',
        '  -H "Content-Type: application/json" \\',
        `  -d '{"model":"${model}","max_tokens":256,"messages":[{"role":"user","content":"Say hello"}]}'`,
      ].join('\n'), note: 'Anthropic style. Authorization: Bearer works in place of x-api-key.' });
    return list;
  }

  W.APISETUP = { setups, originOf, pickModel, NO_KEY, NO_MODEL };
})();
