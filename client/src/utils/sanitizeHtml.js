import DOMPurify from 'dompurify';

const ALLOWED_TAGS = [
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
];

const ALLOWED_ATTRIBUTES = {
  a: new Set(['href', 'title', 'target', 'rel']),
  span: new Set(['class']),
  small: new Set(['class']),
  code: new Set(['class'])
};

const SAFE_URL_PATTERN = /^(https?:|mailto:|#|\/)/i;
const CLASS_ALLOWED_TAGS = new Set(['span', 'small', 'code']);
let hooksInstalled = false;

function installHooks() {
  if (hooksInstalled) return;

  DOMPurify.addHook('uponSanitizeAttribute', (node, data) => {
    const tagName = node.tagName?.toLowerCase();
    const attrName = data.attrName?.toLowerCase();
    if (!tagName || !attrName) {
      data.keepAttr = false;
      return;
    }

    const allowedAttrs = ALLOWED_ATTRIBUTES[tagName] || new Set();
    if (!allowedAttrs.has(attrName) || attrName.startsWith('on')) {
      data.keepAttr = false;
      return;
    }

    if (attrName === 'href' && !SAFE_URL_PATTERN.test((data.attrValue || '').trim())) {
      data.keepAttr = false;
      return;
    }

    if (attrName === 'class' && !CLASS_ALLOWED_TAGS.has(tagName)) {
      data.keepAttr = false;
      return;
    }

    if (attrName === 'target' && data.attrValue !== '_blank') {
      data.keepAttr = false;
    }
  });

  DOMPurify.addHook('afterSanitizeAttributes', (node) => {
    if (node.tagName?.toLowerCase() === 'a') {
      node.setAttribute('rel', 'noopener noreferrer');
    }
  });

  hooksInstalled = true;
}

export function sanitizeHtml(html) {
  if (typeof html !== 'string' || !html) {
    return '';
  }

  installHooks();
  return DOMPurify.sanitize(html, {
    ALLOWED_TAGS,
    ALLOWED_ATTR: ['href', 'title', 'target', 'rel', 'class'],
    ALLOW_DATA_ATTR: false,
    ALLOW_UNKNOWN_PROTOCOLS: false,
    RETURN_TRUSTED_TYPE: false
  });
}
