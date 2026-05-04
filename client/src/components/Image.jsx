import { useState, useCallback, useEffect, useRef } from 'react';
import axios from 'axios'

/**
 * 带重试功能的图片组件
 * @param {string} src - 图片源地址
 * @param {string} alt - 图片描述
 * @param {number} maxRetries - 最大重试次数，默认3次
 * @param {number} retryDelay - 重试延迟（毫秒），默认1000ms
 * @param {string} fallbackSrc - 加载失败后的备用图片
 * @param {object} props - 其他传递给img标签的属性
 */
function Image({ 
  src, 
  alt = '', 
  maxRetries = 10, 
  retryDelay = 5000, 
  fallbackSrc = '/assets/icon.jpg',
  preferSource = false,
  cachedOnly = false,
  onLoadSuccess,
  onLoadError,
  className = '',
  ...props 
}) {
  const [currentSrc, setCurrentSrc] = useState(src);
  const [retryCount, setRetryCount] = useState(0);
  const [isLoading, setIsLoading] = useState(true);
  const [hasFailed, setHasFailed] = useState(false);
  const retryTimeoutRef = useRef(null);
  const mountedRef = useRef(true);

  // 当src改变时重置状态
  useEffect(() => {
    mountedRef.current = true;
    setCurrentSrc(src);
    setRetryCount(0);
    setIsLoading(true);
    setHasFailed(false);
    
    return () => {
      mountedRef.current = false;
      if (retryTimeoutRef.current) {
        clearTimeout(retryTimeoutRef.current);
      }
    };
  }, [src]);

  // If src points to our `/img/:id.webp` or `/img/subject/:id.webp` proxy, resolve it via server first.
  // This allows the server to attempt caching; if it can't fetch within the
  // configured time window, we fall back to direct-origin URL so the browser
  // can try loading it (client shows placeholder meanwhile).
  useEffect(() => {
    const m = typeof src === 'string' ? src.match(/\/img\/(?:(subject)\/)?(\d+)\.webp(?:\?.*)?$/) : null
    if (!m) return

    const isSubject = m[1] === 'subject'
    const id = m[2]
    let cancelled = false

    async function resolve() {
      try {
        // Avoid immediately hitting `/img/:id.webp` before the server has a chance
        // to resolve/cache; show placeholder while we ask the server.
        if (fallbackSrc) setCurrentSrc(fallbackSrc)

        const base = import.meta.env.VITE_SERVER_URL || (typeof window !== 'undefined' ? window.location.origin : '')
        const url = preferSource
          ? (isSubject ? `${base}/api/img/source/subject/${id}` : `${base}/api/img/source/${id}`)
          : (isSubject ? `${base}/api/img/resolve/subject/${id}` : `${base}/api/img/resolve/${id}`)
        const res = await axios.get(url, {
          timeout: cachedOnly ? 500 : 1500,
          params: cachedOnly ? { cachedOnly: 1, waitMs: 0 } : undefined,
        })
        if (cancelled || !mountedRef.current) return

        // Server returns JSON; prefer proxy url when cached, otherwise try sourceUrl.
        const data = res.data || {}
        if (preferSource && data.sourceUrl) {
          setCurrentSrc(data.sourceUrl)
        } else if (data.cached) {
          setCurrentSrc(data.imgUrl || src)
        } else if (data.sourceUrl) {
          // Show placeholder immediately, then try the origin URL directly.
          // This matches the "server couldn't fetch within time window" UX goal.
          setTimeout(() => {
            if (cancelled || !mountedRef.current) return
            setCurrentSrc(data.sourceUrl)
          }, 0)
        }
      } catch {
        // If resolve fails (server down), fall back to trying the original src.
        // Normal retry logic will handle errors.
        setCurrentSrc(src)
      }
    }

    resolve()
    return () => { cancelled = true }
  }, [src, fallbackSrc, preferSource, cachedOnly]);

  const handleError = useCallback(() => {
    if (!mountedRef.current) return;

    if (retryCount < maxRetries) {
      // 还有重试机会，延迟后重试
      const nextRetry = retryCount + 1;
      console.log(`[Image] 图片加载失败，正在重试 (${nextRetry}/${maxRetries}): ${src}`);
      
      retryTimeoutRef.current = setTimeout(() => {
        if (!mountedRef.current) return;
        setRetryCount(nextRetry);
        // 添加时间戳绕过缓存
        const baseSrc = currentSrc || src
        const separator = baseSrc.includes('?') ? '&' : '?';
        setCurrentSrc(`${baseSrc}${separator}_retry=${Date.now()}`);
      }, retryDelay * nextRetry); // 指数退避
    } else {
      // 已达到最大重试次数
      console.warn(`[Image] 图片加载失败，已达到最大重试次数: ${src}`);
      setIsLoading(false);
      setHasFailed(true);
      
      if (fallbackSrc) {
        setCurrentSrc(fallbackSrc);
      }
      
      if (onLoadError) {
        onLoadError(new Error(`Failed to load image after ${maxRetries} retries: ${src}`));
      }
    }
  }, [src, currentSrc, retryCount, maxRetries, retryDelay, fallbackSrc, onLoadError]);

  const handleLoad = useCallback(() => {
    if (!mountedRef.current) return;
    setIsLoading(false);
    setHasFailed(false);
    if (onLoadSuccess) {
      onLoadSuccess();
    }
  }, [onLoadSuccess]);

  return (
    <img
      src={currentSrc}
      alt={alt}
      onError={handleError}
      onLoad={handleLoad}
      className={`${className}${isLoading ? ' is-loading' : ''}${hasFailed ? ' has-failed' : ''}`.trim() || undefined}
      {...props}
    />
  );
}

export default Image;
