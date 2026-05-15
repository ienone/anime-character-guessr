// @vitest-environment jsdom
import { describe, expect, it } from 'vitest';
import { sanitizeHtml } from './sanitizeHtml';

function renderSanitized(html) {
  const container = document.createElement('div');
  container.innerHTML = sanitizeHtml(html);
  return container;
}

describe('sanitizeHtml', () => {
  it('strips scripts, unsupported elements, and event handlers', () => {
    const container = renderSanitized(`
      <script>alert(1)</script>
      <img src=x onerror=alert(1)>
      <b onclick="alert(1)">safe</b>
    `);

    expect(container.querySelector('script')).toBeNull();
    expect(container.querySelector('img')).toBeNull();
    expect(container.querySelector('b')?.textContent).toBe('safe');
    expect(container.querySelector('b')?.hasAttribute('onclick')).toBe(false);
  });

  it('keeps allowed formatting while enforcing per-tag attributes', () => {
    const container = renderSanitized(`
      <p class="wrong" style="color:red">
        text <span class="tag-pill" style="color:red">tag</span>
        <code class="inline-code">code</code>
      </p>
    `);

    const paragraph = container.querySelector('p');
    const span = container.querySelector('span');
    const code = container.querySelector('code');

    expect(paragraph?.hasAttribute('class')).toBe(false);
    expect(paragraph?.hasAttribute('style')).toBe(false);
    expect(span?.getAttribute('class')).toBe('tag-pill');
    expect(span?.hasAttribute('style')).toBe(false);
    expect(code?.getAttribute('class')).toBe('inline-code');
  });

  it('blocks unsafe href protocols and preserves safe links with noopener rel', () => {
    const container = renderSanitized(`
      <a href="javascript:alert(1)" target="_blank">bad</a>
      <a href="https://example.com" target="_blank">web</a>
      <a href="/local/path">local</a>
      <a href="mailto:test@example.com">mail</a>
    `);
    const anchors = Array.from(container.querySelectorAll('a'));

    expect(anchors).toHaveLength(4);
    expect(anchors[0].hasAttribute('href')).toBe(false);
    expect(anchors[1].getAttribute('href')).toBe('https://example.com');
    expect(anchors[1].getAttribute('target')).toBe('_blank');
    expect(anchors[1].getAttribute('rel')).toBe('noopener noreferrer');
    expect(anchors[2].getAttribute('href')).toBe('/local/path');
    expect(anchors[3].getAttribute('href')).toBe('mailto:test@example.com');
  });

  it('unwraps unsupported tags instead of dropping safe text content', () => {
    const container = renderSanitized('<section><em>keep</em><iframe src="x"></iframe></section>');

    expect(container.textContent).toBe('keep');
    expect(container.querySelector('em')?.textContent).toBe('keep');
    expect(container.querySelector('section')).toBeNull();
    expect(container.querySelector('iframe')).toBeNull();
  });
});
