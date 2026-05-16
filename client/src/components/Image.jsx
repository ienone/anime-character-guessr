import { useState, useCallback, useEffect, useRef } from 'react';
import axios from 'axios'

const IMAGE_PROXY_RE = /\/img\/(?:(subject)\/)?(?:(medium)\/)?(\d+)\.webp(?:\?.*)?$/

function parseImageProxySrc(src) {
  const match = typeof src === 'string' ? src.match(IMAGE_PROXY_RE) : null
  if (!match) return null

  return {
    isSubject: match[1] === 'subject',
    isMedium: match[2] === 'medium',
    id: match[3],
  }
}

function initialImageSrc(src, fallbackSrc) {
  return parseImageProxySrc(src) ? (fallbackSrc || undefined) : src
}

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
  maxRetries = 2,
  retryDelay = 1000,
  fallbackSrc = '/assets/icon.jpg',
  preferSource = false,
  cachedOnly = false,
  variant,
  waitMs,
  sourceFallback = true,
  onLoadSuccess,
  onLoadError,
  className = '',
  ...props 
}) {
  const [currentSrc, setCurrentSrc] = useState(() => initialImageSrc(src, fallbackSrc));
  const [retryCount, setRetryCount] = useState(0);
  const [isLoading, setIsLoading] = useState(true);
  const [hasFailed, setHasFailed] = useState(false);
  const retryTimeoutRef = useRef(null);
  const mountedRef = useRef(true);

  // 当src改变时重置状态
  useEffect(() => {
    mountedRef.current = true;
    setCurrentSrc(initialImageSrc(src, fallbackSrc));
    setRetryCount(0);
    setIsLoading(true);
    setHasFailed(false);
    
    return () => {
      mountedRef.current = false;
      if (retryTimeoutRef.current) {
        clearTimeout(retryTimeoutRef.current);
      }
    };
  }, [src, fallbackSrc]);

  // If src points to our image proxy, ask the server to resolve/warm the cache
  // before loading it. The server may return an upstream fallback while the
  // background cache fill continues.
  useEffect(() => {
    const parsed = parseImageProxySrc(src)
    if (!parsed) return

    const resolvedVariant = variant || (parsed.isMedium || preferSource ? 'medium' : 'grid')
    const resolvedWaitMs = cachedOnly ? 0 : (waitMs ?? (resolvedVariant === 'medium' ? 1200 : 900))
    let cancelled = false
    let resolveDelayId = null

    async function resolve() {
      try {
        const base = import.meta.env.VITE_SERVER_URL || (typeof window !== 'undefined' ? window.location.origin : '')
        const apiBase = base.replace(/\/$/, '')
        const url = parsed.isSubject ? `${apiBase}/api/img/resolve/subject/${parsed.id}` : `${apiBase}/api/img/resolve/${parsed.id}`
        const res = await axios.get(url, {
          timeout: cachedOnly ? 500 : Math.max(1500, Number(resolvedWaitMs) + 800),
          params: {
            variant: resolvedVariant,
            waitMs: resolvedWaitMs,
            fallback: sourceFallback ? 'source' : 'none',
            ...(cachedOnly ? { cachedOnly: 1 } : {}),
          },
        })
        if (cancelled || !mountedRef.current) return

        const data = res.data || {}
        if (data.cached) {
          setCurrentSrc(data.imgUrl || src)
        } else if (!cachedOnly && data.sourceUrl) {
          setCurrentSrc(data.sourceUrl)
        } else if (!cachedOnly && data.imgUrl) {
          const retryDelayMs = preferSource ? 400 : 800
          resolveDelayId = setTimeout(() => {
            if (cancelled || !mountedRef.current) return
            setCurrentSrc(data.imgUrl)
          }, retryDelayMs)
        }
      } catch {
        // If resolve fails (server down), fall back to trying the original src.
        // Normal retry logic will handle errors.
        if (cancelled || !mountedRef.current) return
        setCurrentSrc(src)
      }
    }

    resolve()
    return () => {
      cancelled = true
      if (resolveDelayId) clearTimeout(resolveDelayId)
    }
  }, [src, preferSource, cachedOnly, variant, waitMs, sourceFallback]);

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
