const ALLOWED_TAGS = new Set([
  'a',
  'br',
  'b',
  'strong',
  'i',
  'em',
  'u',
  'span',
  'small',
  'code',
  'ul',
  'ol',
  'li',
  'p'
]);

const ALLOWED_ATTRIBUTES = {
  a: new Set(['href', 'title', 'target', 'rel']),
  span: new Set(['class']),
  small: new Set(['class']),
  code: new Set(['class'])
};

const SAFE_URL_PATTERN = /^(https?:|mailto:|#|\/)/i;

function sanitizeNode(document, node) {
  if (node.nodeType === Node.TEXT_NODE) {
    return document.createTextNode(node.textContent || '');
  }

  if (node.nodeType !== Node.ELEMENT_NODE) {
    return document.createTextNode('');
  }

  const tagName = node.tagName.toLowerCase();
  if (!ALLOWED_TAGS.has(tagName)) {
    const fragment = document.createDocumentFragment();
    node.childNodes.forEach(child => {
      fragment.appendChild(sanitizeNode(document, child));
    });
    return fragment;
  }

  const element = document.createElement(tagName);
  const allowedAttrs = ALLOWED_ATTRIBUTES[tagName] || new Set();
  Array.from(node.attributes).forEach(attr => {
    const name = attr.name.toLowerCase();
    const value = attr.value || '';
    if (!allowedAttrs.has(name) || name.startsWith('on')) {
      return;
    }
    if (name === 'href' && !SAFE_URL_PATTERN.test(value.trim())) {
      return;
    }
    element.setAttribute(name, value);
  });

  if (tagName === 'a') {
    element.setAttribute('rel', 'noopener noreferrer');
    if (element.getAttribute('target') === '_blank') {
      element.setAttribute('target', '_blank');
    }
  }

  node.childNodes.forEach(child => {
    element.appendChild(sanitizeNode(document, child));
  });
  return element;
}

export function sanitizeHtml(html) {
  if (typeof html !== 'string' || !html) {
    return '';
  }

  const parser = new DOMParser();
  const parsed = parser.parseFromString(html, 'text/html');
  const fragment = document.createDocumentFragment();
  parsed.body.childNodes.forEach(child => {
    fragment.appendChild(sanitizeNode(document, child));
  });

  const container = document.createElement('div');
  container.appendChild(fragment);
  return container.innerHTML;
}
