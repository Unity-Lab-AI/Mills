// Stub plugin for the editor foundation (T-205): "page elements", a few boxes for the website builder (T-191). It exists
// to prove the foundation is not workflow-specific: boxes have no ports, a size and some style properties. The real
// element set, slots and the inherit / override style layer come with T-191. The core does not reference this file.
(function (root, factory) {
  const api = factory();
  if (typeof module === 'object' && module.exports) module.exports = api;
  else root.VrKindsBoxes = api;
})(typeof self !== 'undefined' ? self : this, function () {
  'use strict';
  const colour = { type: 'string', title: 'Colour', pattern: '^#[0-9a-fA-F]{3,8}$', message: 'A colour like #4a7' };
  const E = (id, title, size, props, color) => ({ id, title, category: 'page elements', size, ports: { in: [], out: [] }, schema: { type: 'object', properties: props }, render: Object.assign({ color, resizable: true }, id === 'container' ? { shape: 'frame' } : {}) });
  const plugin = {
    id: 'boxes', title: 'Page elements (stub)',
    kinds: [
      E('heading', 'Heading', [320, 60], { text: { type: 'string', title: 'Text', maxLength: 200 }, level: { type: 'integer', title: 'Level', min: 1, max: 6 }, color: colour }, '#7c9cff'),
      E('text', 'Text block', [320, 120], { text: { type: 'string', title: 'Text', maxLength: 5000 }, color: colour }, '#8b93a7'),
      E('image', 'Image', [240, 160], { src: { type: 'string', title: 'Image address', maxLength: 500 }, alt: { type: 'string', title: 'Description', maxLength: 200 } }, '#4fd18b'),
      E('button', 'Button', [140, 48], { text: { type: 'string', title: 'Text', maxLength: 60 }, href: { type: 'string', title: 'Link', maxLength: 500 }, color: colour }, '#e6a14f'),
      E('container', 'Container', [400, 240], { color: colour, padding: { type: 'integer', title: 'Padding', min: 0, max: 200 } }, '#2a2f3a'),
    ],
  };
  return { id: 'boxes', plugin, register: (registry) => registry.register(plugin) };
});
