console.log('[runtime-bootstrap] loaded');
(function () {
  var SUPPORTED_LOCALES = {
    'ar': 'العربية', 'ar-AE': 'العربية (الإمارات)', 'ar-DZ': 'العربية (الجزائر)',
    'ar-EG': 'العربية (مصر)', 'ar-IQ': 'العربية (العراق)', 'ar-MA': 'العربية (المغرب)',
    'ar-SA': 'العربية (السعودية)', 'ar-SD': 'العربية (السودان)', 'ar-SY': 'العربية (سوريا)',
    'ar-TN': 'العربية (تونس)',
    'ay': 'Aymar aru',
    'bg': 'Български',
    'bn': 'বাংলা',
    'cs': 'Čeština',
    'da': 'Dansk',
    'de': 'Deutsch',
    'el': 'Ελληνικά',
    'en': 'English',
    'en-GB': 'English (UK)',
    'es': 'Español',
    'et': 'Eesti',
    'fa': 'فارسی',
    'fi': 'Suomi',
    'fr': 'Français',
    'gn': "Avañe'ẽ",
    'gu': 'ગુજરાતી',
    'hi': 'हिन्दी',
    'hr': 'Hrvatski',
    'ht': 'Kreyòl ayisyen',
    'hu': 'Magyar',
    'id': 'Bahasa Indonesia',
    'it': 'Italiano',
    'ja': '日本語',
    'km': 'ខ្មែរ',
    'kn': 'ಕನ್ನಡ',
    'ko': '한국어',
    'lo': 'ລາວ',
    'lt': 'Lietuvių',
    'lv': 'Latviešu',
    'ml': 'മലയാളം',
    'mr': 'मराठी',
    'ms': 'Bahasa Melayu',
    'my': 'မြန်မာ',
    'nah': 'Nāhuatl',
    'ne': 'नेपाली',
    'nl': 'Nederlands',
    'no': 'Norsk',
    'pa': 'ਪੰਜਾਬੀ',
    'pl': 'Polski',
    'pt': 'Português',
    'qu': 'Runa simi',
    'ro': 'Română',
    'ru': 'Русский',
    'si': 'සිංහල',
    'sk': 'Slovenčina',
    'sr': 'Српски',
    'sv': 'Svenska',
    'ta': 'தமிழ்',
    'te': 'తెలుగు',
    'th': 'ไทย',
    'tl': 'Tagalog',
    'tr': 'Türkçe',
    'uk': 'Українська',
    'ur': 'اردو',
    'vi': 'Tiếng Việt',
    'zh': '中文'
  };

  // ---------------------------------------------------------------------------
  // Tenant / env / locale resolution
  // ---------------------------------------------------------------------------

  function resolveTenant() {
    var match = window.location.pathname.match(/\/v1\/web\/webchat\/([^\/?#]+)/i);
    if (match && match[1]) {
      return decodeURIComponent(match[1]);
    }
    var queryTenant = new URLSearchParams(window.location.search).get('tenant');
    if (queryTenant) {
      return queryTenant;
    }
    return document.documentElement?.dataset?.tenant || 'default';
  }

  function resolveEnv() {
    var queryEnv = new URLSearchParams(window.location.search).get('env');
    if (queryEnv) {
      return queryEnv;
    }
    return document.documentElement?.dataset?.env || 'default';
  }

  function resolveLocale() {
    var queryLang = new URLSearchParams(window.location.search).get('lang');
    if (queryLang && SUPPORTED_LOCALES[queryLang]) {
      return queryLang;
    }
    return null;
  }

  // Segments after {tenant} that are reserved API prefixes and must never be
  // mistaken for a bundle_id or flow_id.
  var RESERVED_WEBCHAT_SEGMENTS = Object.create(null);
  RESERVED_WEBCHAT_SEGMENTS['token'] = 1;
  RESERVED_WEBCHAT_SEGMENTS['v3'] = 1;
  RESERVED_WEBCHAT_SEGMENTS['oauth'] = 1;

  // Parse the optional bundle and flow segments from the page URL.
  // The server is authoritative; the client uses a defensive heuristic:
  //   /v1/web/webchat/{tenant}[/{bundle_id}[/{flow_id}]]
  // A segment that is a reserved literal or contains a '.' (file extension)
  // stops the parse — everything beyond is an asset path.
  function resolveWebchatSegments(pathname) {
    var m = pathname.match(
      /\/v1\/web\/webchat\/([^\/?#]+)(?:\/([^\/?#]+))?(?:\/([^\/?#]+))?/i
    );
    if (!m || !m[1]) return {bundleId: null, flowId: null};
    var bundleId = null;
    var flowId = null;
    if (m[2]) {
      var seg2 = decodeURIComponent(m[2]);
      if (!RESERVED_WEBCHAT_SEGMENTS[seg2.toLowerCase()] && seg2.indexOf('.') === -1) {
        bundleId = seg2;
        if (m[3]) {
          var seg3 = decodeURIComponent(m[3]);
          if (!RESERVED_WEBCHAT_SEGMENTS[seg3.toLowerCase()] && seg3.indexOf('.') === -1) {
            flowId = seg3;
          }
        }
      }
    }
    return {bundleId: bundleId, flowId: flowId};
  }

  function resolveGuiBase(tenant, bundleId) {
    var base = '/v1/web/webchat/' + encodeURIComponent(tenant) + '/';
    if (bundleId) {
      base += encodeURIComponent(bundleId) + '/';
    }
    return base;
  }

  function backendBase(tenant) {
    return window.location.origin + '/v1/messaging/webchat/' + encodeURIComponent(tenant);
  }

  function normalizeAdaptiveCardWidth(value) {
    var raw = value == null ? '' : String(value).trim();
    if (!raw) return '70%';
    if (/^\d+(?:\.\d+)?$/.test(raw)) return raw + '%';
    if (/^\d+(?:\.\d+)?(?:%|px|rem|em|vw|vh)$/.test(raw)) return raw;
    if (raw.toLowerCase() === 'auto') return 'auto';
    return '70%';
  }

  function resolveAdaptiveCardWidth() {
    var params = new URLSearchParams(window.location.search);
    return normalizeAdaptiveCardWidth(
      window.__GREENTIC_WEBCHAT_ADAPTIVE_CARD_WIDTH__ ||
      params.get('adaptiveCardWidth') ||
      params.get('adaptive_card_width')
    );
  }

  function ensureAdaptiveCardWidthStyle() {
    var width = resolveAdaptiveCardWidth();
    document.documentElement.style.setProperty('--greentic-adaptive-card-width', width);
    var existing = document.getElementById('greentic-adaptive-card-width-style');
    if (existing) return;
    var style = document.createElement('style');
    style.id = 'greentic-adaptive-card-width-style';
    style.textContent = [
      '.tenant-widget-surface .ac-adaptiveCard, .embed-webchat-surface .ac-adaptiveCard, .widget-surface .ac-adaptiveCard, .chat-panel__surface .ac-adaptiveCard {',
      '  width: min(100%, var(--greentic-adaptive-card-width, 70%)) !important;',
      '  max-width: min(100%, var(--greentic-adaptive-card-width, 70%)) !important;',
      '  box-sizing: border-box !important;',
      '}',
      '.tenant-widget-surface .webchat__bubble__content, .embed-webchat-surface .webchat__bubble__content, .widget-surface .webchat__bubble__content, .chat-panel__surface .webchat__bubble__content {',
      '  max-width: 100% !important;',
      '  box-sizing: border-box !important;',
      '}',
      '.greentic-adaptive-card-bubble {',
      '  width: min(100%, var(--greentic-adaptive-card-width, 70%)) !important;',
      '  max-width: min(100%, var(--greentic-adaptive-card-width, 70%)) !important;',
      '  box-sizing: border-box !important;',
      '}',
      '.greentic-adaptive-card-bubble .ac-adaptiveCard {',
      '  width: 100% !important;',
      '  max-width: 100% !important;',
      '  box-sizing: border-box !important;',
      '}'
    ].join('\n');
    document.head.appendChild(style);
  }

  function markAdaptiveCardBubbles(root) {
    var scope = root && root.querySelectorAll ? root : document;
    var cards = scope.querySelectorAll('.ac-adaptiveCard');
    for (var i = 0; i < cards.length; i++) {
      var bubble = cards[i].closest && cards[i].closest('.webchat__bubble__content');
      if (bubble) {
        var width = 'min(100%, var(--greentic-adaptive-card-width, 70%))';
        bubble.classList.add('greentic-adaptive-card-bubble');
        bubble.style.setProperty('width', width, 'important');
        bubble.style.setProperty('max-width', width, 'important');
        bubble.style.setProperty('box-sizing', 'border-box', 'important');
        cards[i].style.setProperty('width', '100%', 'important');
        cards[i].style.setProperty('max-width', '100%', 'important');
        cards[i].style.setProperty('box-sizing', 'border-box', 'important');
      }
    }
  }

  function ensureAdaptiveCardWidthObserver() {
    markAdaptiveCardBubbles(document);
    if (window.__GREENTIC_ADAPTIVE_CARD_WIDTH_OBSERVER__) return;
    window.__GREENTIC_ADAPTIVE_CARD_WIDTH_OBSERVER__ = true;
    if (typeof MutationObserver === 'undefined') return;
    new MutationObserver(function (mutations) {
      for (var i = 0; i < mutations.length; i++) {
        for (var j = 0; j < mutations[i].addedNodes.length; j++) {
          var node = mutations[i].addedNodes[j];
          if (node && node.nodeType === 1) markAdaptiveCardBubbles(node);
        }
      }
    }).observe(document.documentElement, { childList: true, subtree: true });
  }

  var tenant = resolveTenant();
  var env = resolveEnv();
  var selectedLocale = resolveLocale();
  var segments = resolveWebchatSegments(window.location.pathname);
  var bundleId = segments.bundleId;
  var flowId = segments.flowId;
  var guiBase = resolveGuiBase(tenant, bundleId);
  console.log('[runtime-bootstrap] tenant:', tenant, 'env:', env,
    'locale:', selectedLocale || '(default)',
    'bundle:', bundleId || '(default)', 'flow:', flowId || '(none)');
  document.documentElement.style.setProperty('--greentic-adaptive-card-width', resolveAdaptiveCardWidth());
  setTimeout(function () {
    ensureAdaptiveCardWidthStyle();
    ensureAdaptiveCardWidthObserver();
  }, 0);

  // Detect OAuth completion redirect (?oauth_done=true)
  var oauthDone = new URLSearchParams(window.location.search).get('oauth_done') === 'true';
  if (oauthDone) {
    // Clean URL
    var cleanUrl = window.location.pathname;
    window.history.replaceState({}, '', cleanUrl);
    // Set initial message so webchat auto-sends it on connect
    window.__INITIAL_MESSAGE__ = 'oauth_login_success';
    console.log('[runtime-bootstrap] OAuth done, will auto-send oauth_login_success');
  }

  document.documentElement.dataset.tenant = tenant;
  window.__TENANT__ = tenant;
  window.__BASE_PATH__ = guiBase;
  window.APP_CONFIG_BASE = './config';
  window.__WEBCHAT_GUI_BASE__ = guiBase;
  window.__WEBCHAT_BACKEND_BASE__ = backendBase(tenant);
  window.__BUNDLE_ID__ = bundleId;
  window.__FLOW_ID__ = flowId;
  window.__SUPPORTED_LOCALES__ = SUPPORTED_LOCALES;
  window.__SELECTED_LOCALE__ = selectedLocale;

  // ---------------------------------------------------------------------------
  // Guest identity: stable per-browser UUID used as the rate-limit subject on
  // the Direct Line /token endpoint. Without it, every anonymous visitor
  // shares the server-side `anonymous` bucket, so any reasonable amount of
  // concurrent traffic instantly trips the 429 cap.
  // ---------------------------------------------------------------------------
  var GUEST_ID_KEY = 'greentic_guest_id';
  function generateGuestId() {
    try {
      if (window.crypto && typeof window.crypto.randomUUID === 'function') {
        return window.crypto.randomUUID();
      }
    } catch (_) {}
    // Fallback for older browsers: RFC 4122 v4 shape using Math.random.
    return 'gxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx'.replace(/[xy]/g, function (c) {
      var r = (Math.random() * 16) | 0;
      var v = c === 'x' ? r : (r & 0x3) | 0x8;
      return v.toString(16);
    });
  }
  function resolveGuestId() {
    try {
      var existing = localStorage.getItem(GUEST_ID_KEY);
      if (existing && existing.length > 0) return existing;
      var fresh = generateGuestId();
      localStorage.setItem(GUEST_ID_KEY, fresh);
      return fresh;
    } catch (_) {
      // localStorage blocked (private mode / disabled) — generate per-load
      // ID so we still split server buckets, even if not stable across reloads.
      return generateGuestId();
    }
  }
  var guestId = resolveGuestId();
  window.__GUEST_ID__ = guestId;
  console.log('[runtime-bootstrap] guest id:', guestId);

  // ---------------------------------------------------------------------------
  // UI i18n: load translations for chrome strings (Logout, WebChat title, etc.)
  // ---------------------------------------------------------------------------

  var UI_STRINGS = {};
  var UI_STRINGS_LOADED = false;
  var UI_STRINGS_CALLBACKS = [];

  function loadUiI18n(locale) {
    var lang = (locale || 'en').replace(/[^a-zA-Z0-9-]/g, '') || 'en';
    var url = guiBase + 'i18n/' + lang + '.json';
    // GUI i18n files are resolved from the packaged GUI base path with a sanitized locale.
    // foxguard: ignore[js/no-ssrf]
    fetch(url).then(function (res) {
      if (!res.ok) {
        // Fall back to base language (ar-AE -> ar)
        var base = lang.split('-')[0];
        if (base !== lang) {
          // GUI i18n files are resolved from the packaged GUI base path with a sanitized locale.
          // foxguard: ignore[js/no-ssrf]
          return fetch(guiBase + 'i18n/' + base + '.json');
        }
        // Static packaged fallback locale.
        // foxguard: ignore[js/no-ssrf]
        return fetch(guiBase + 'i18n/en.json');
      }
      return res;
    }).then(function (res) {
      if (res && res.ok) return res.json();
      return {};
    }).then(function (data) {
      UI_STRINGS = data || {};
      UI_STRINGS_LOADED = true;
      UI_STRINGS_CALLBACKS.forEach(function (cb) { cb(); });
      UI_STRINGS_CALLBACKS = [];
      applyUiTranslations();
    }).catch(function () {
      UI_STRINGS_LOADED = true;
      UI_STRINGS_CALLBACKS.forEach(function (cb) { cb(); });
      UI_STRINGS_CALLBACKS = [];
    });
  }

  function uiT(key, fallback) {
    return UI_STRINGS[key] || fallback || key;
  }

  function isRtlLocale(locale) {
    var base = (locale || '').split('-')[0];
    return ['ar', 'he', 'fa', 'ur'].indexOf(base) >= 0;
  }

  function applyUiTranslations() {
    // Set topbar title from skin brand.name, fall back to i18n, then 'AI Assistant'
    var titleEl = document.querySelector('.topbar__title');
    if (titleEl) {
      var brandName = (window.__SKIN__ && window.__SKIN__.brand && window.__SKIN__.brand.name) || '';
      titleEl.textContent = brandName || uiT('product.greentic.long', 'AI Assistant');
    }
    // Translate logout button if already injected
    var logoutBtn = document.getElementById('greentic-logout-btn');
    if (logoutBtn) {
      logoutBtn.textContent = uiT('header.logout', 'Logout');
    }
    // Set lang/dir on html element for RTL locales
    var lang = selectedLocale || 'en';
    document.documentElement.lang = lang;
    var rtl = isRtlLocale(lang);
    document.documentElement.dir = rtl ? 'rtl' : 'ltr';

    // Inject RTL CSS for Adaptive Card content when locale is RTL
    if (rtl && !document.getElementById('greentic-rtl-style')) {
      var style = document.createElement('style');
      style.id = 'greentic-rtl-style';
      style.textContent = [
        '[dir="rtl"] .ac-adaptiveCard, [dir="rtl"] .ac-container { direction: rtl; text-align: right; }',
        '[dir="rtl"] .ac-textBlock { direction: rtl; text-align: right; }',
        '[dir="rtl"] .ac-actionSet { direction: rtl; }',
        '[dir="rtl"] .ac-input { direction: rtl; text-align: right; }',
        '[dir="rtl"] .webchat__bubble__content { direction: rtl; }',
        '[dir="rtl"] .webchat__stacked-layout { direction: rtl; }',
        '[dir="rtl"] .topbar { flex-direction: row-reverse; }',
        '[dir="rtl"] .topbar__brand { flex-direction: row-reverse; }',
      ].join('\n');
      document.head.appendChild(style);
    }

    // Baseline pending state for Adaptive Card actions across all skins.
    if (!document.getElementById('greentic-ac-action-pending-style')) {
      var pendingStyle = document.createElement('style');
      pendingStyle.id = 'greentic-ac-action-pending-style';
      pendingStyle.textContent = [
        '.ac-actionSet button[disabled], .ac-actionSet button[aria-disabled="true"], .ac-actionSet button[aria-busy="true"], .ac-pushButton[disabled], .ac-pushButton[aria-disabled="true"], .ac-pushButton[aria-busy="true"] { position: relative !important; cursor: wait !important; opacity: .72 !important; padding-left: 2.125rem !important; pointer-events: none !important; }',
        '.ac-actionSet button[disabled]::before, .ac-actionSet button[aria-disabled="true"]::before, .ac-actionSet button[aria-busy="true"]::before, .ac-pushButton[disabled]::before, .ac-pushButton[aria-disabled="true"]::before, .ac-pushButton[aria-busy="true"]::before { content: "" !important; position: absolute !important; left: .75rem !important; top: 50% !important; width: .875rem !important; height: .875rem !important; margin-top: -.4375rem !important; border: 2px solid currentColor !important; border-right-color: transparent !important; border-radius: 999px !important; animation: ac-action-spin .75s linear infinite !important; box-sizing: border-box !important; }',
        '@keyframes ac-action-spin { to { transform: rotate(360deg); } }',
      ].join('\n');
      document.head.appendChild(pendingStyle);
    }
    if (!window.__greenticAdaptiveCardActionPendingInstalled) {
      window.__greenticAdaptiveCardActionPendingInstalled = true;
      document.addEventListener('click', function (event) {
        var source = event.target;
        var actionButton = source && source.closest
          ? source.closest('.ac-pushButton, .ac-actionSet button')
          : null;
        if (!actionButton || actionButton.getAttribute('aria-busy') === 'true') return;

        window.setTimeout(function () {
          if (!document.documentElement.contains(actionButton)) return;
          actionButton.setAttribute('aria-busy', 'true');
          actionButton.setAttribute('aria-disabled', 'true');
          if ('disabled' in actionButton) actionButton.disabled = true;
        }, 0);
      });
    }
  }

  loadUiI18n(selectedLocale);

  // ---------------------------------------------------------------------------
  // OAuth helper functions
  // ---------------------------------------------------------------------------

  // Storage key namespace, not credential material.
  // foxguard: ignore[js/no-hardcoded-secret]
  var OAUTH_STORAGE_PREFIX = 'greentic_oauth_';

  function oauthStorageKey(key) {
    return OAUTH_STORAGE_PREFIX + key;
  }

  function trustedHttpUrl(value) {
    var url = new URL(value, window.location.href);
    if (url.protocol !== 'https:' && url.protocol !== 'http:') {
      throw new Error('unsupported URL protocol');
    }
    return url.toString();
  }

  function sameOriginUrl(value) {
    var url = new URL(value, window.location.href);
    if (url.origin !== window.location.origin) {
      throw new Error('cross-origin backend URL');
    }
    return url.toString();
  }

  function getOAuthSession() {
    try {
      var handle = sessionStorage.getItem(oauthStorageKey('token_handle'));
      var flowId = sessionStorage.getItem(oauthStorageKey('flow_id'));
      if (handle && flowId) {
        return { token_handle: handle, flow_id: flowId };
      }
    } catch (_) { /* sessionStorage unavailable */ }
    return null;
  }

  function saveOAuthSession(tokenHandle, flowId) {
    try {
      sessionStorage.setItem(oauthStorageKey('token_handle'), tokenHandle);
      sessionStorage.setItem(oauthStorageKey('flow_id'), flowId);
    } catch (_) { /* sessionStorage unavailable */ }
  }

  function clearOAuthSession() {
    try {
      sessionStorage.removeItem(oauthStorageKey('token_handle'));
      sessionStorage.removeItem(oauthStorageKey('flow_id'));
      sessionStorage.removeItem(oauthStorageKey('user_name'));
      sessionStorage.removeItem(oauthStorageKey('user_email'));
      sessionStorage.removeItem(oauthStorageKey('user_picture'));
      sessionStorage.removeItem(oauthStorageKey('provider'));
    } catch (_) { /* sessionStorage unavailable */ }
  }

  function getAppAuthSession() {
    try {
      var raw = localStorage.getItem('webchat_auth_session');
      if (!raw) return null;
      var parsed = JSON.parse(raw);
      return parsed && parsed.isAuthenticated ? parsed : null;
    } catch (_) {
      return null;
    }
  }

  function clearAppAuthSession() {
    try {
      localStorage.setItem('webchat_auth_session', JSON.stringify({ isAuthenticated: false }));
    } catch (_) { /* localStorage unavailable */ }
  }

  function hasAnyAuthSession() {
    return !!(getOAuthSession() || getAppAuthSession());
  }

  /**
   * Check URL for OAuth callback params (?code=...&state=...).
   * If found, exchange code for tokens via PKCE, save session, clean URL.
   * Returns true if callback was detected (async exchange happens in background).
   */
  function handleOAuthCallback() {
    var params = new URLSearchParams(window.location.search);
    var code = params.get('code');
    var state = params.get('state');
    var error = params.get('error');

    if (error) {
      var errorDesc = params.get('error_description') || error;
      console.error('[oauth] provider returned error:', errorDesc);
      cleanCallbackParams(params);
      showAuthError('Authentication failed: ' + errorDesc);
      return true;
    }

    if (!code || !state) {
      return false;
    }

    // Verify state matches what we stored
    var storedState;
    try { storedState = sessionStorage.getItem(oauthStorageKey('state')); } catch (_) {}
    if (storedState && storedState !== state) {
      console.error('[oauth] state mismatch');
      cleanCallbackParams(params);
      showAuthError('Authentication failed: invalid state. Please try again.');
      return true;
    }

    cleanCallbackParams(params);

    // Exchange code for tokens via server-side proxy (avoids CORS)
    var codeVerifier, redirectUri, storedProvider;
    try {
      codeVerifier = sessionStorage.getItem(oauthStorageKey('code_verifier'));
      redirectUri = sessionStorage.getItem(oauthStorageKey('redirect_uri'));
      var providerStr = sessionStorage.getItem(oauthStorageKey('provider'));
      if (providerStr) storedProvider = JSON.parse(providerStr);
    } catch (_) {}

    if (!storedProvider || !storedProvider.token_url || !storedProvider.client_id) {
      // Fallback: save as authenticated without user info
      console.log('[oauth] no provider info, saving basic session');
      saveOAuthSession('authenticated', 'oauth-code');
      removeOAuthOverlay();
      injectLogoutButton();
      return true;
    }

    // Process tokens: decode id_token, save user info, update UI
    function handleTokens(tokens) {
      if (tokens.error) {
        throw new Error(tokens.error_description || tokens.error);
      }
      var userInfo = {};
      if (tokens.id_token) {
        try {
          var parts = tokens.id_token.split('.');
          var payload = JSON.parse(atob(parts[1].replace(/-/g, '+').replace(/_/g, '/')));
          userInfo = {
            name: payload.name || '',
            email: payload.email || '',
            picture: payload.picture || ''
          };
        } catch (_) {}
      }
      console.log('[oauth] authenticated:', userInfo.name || userInfo.email || 'user');
      saveOAuthSession(tokens.id_token || tokens.access_token || 'authenticated', 'oauth-code');
      try {
        if (userInfo.name) sessionStorage.setItem(oauthStorageKey('user_name'), userInfo.name);
        if (userInfo.email) sessionStorage.setItem(oauthStorageKey('user_email'), userInfo.email);
        if (userInfo.picture) sessionStorage.setItem(oauthStorageKey('user_picture'), userInfo.picture);
        sessionStorage.removeItem(oauthStorageKey('code_verifier'));
        sessionStorage.removeItem(oauthStorageKey('redirect_uri'));
        sessionStorage.removeItem(oauthStorageKey('state'));
      } catch (_) {}
      removeOAuthOverlay();
      injectLogoutButton();
    }

    // Direct PKCE token exchange with OAuth provider (no client_secret needed)
    function directTokenExchange() {
      if (!storedProvider.token_url) {
        throw new Error('no token_url');
      }
      var tokenUrl = trustedHttpUrl(storedProvider.token_url);
      console.log('[oauth] trying direct PKCE token exchange with', tokenUrl);
      var body = new URLSearchParams({
        grant_type: 'authorization_code',
        code: code,
        redirect_uri: redirectUri || window.location.href.split('?')[0],
        client_id: storedProvider.client_id,
        code_verifier: codeVerifier || ''
      });
      // OAuth token endpoints are provider-configured HTTPS URLs.
      // foxguard: ignore[js/no-ssrf]
      return fetch(tokenUrl, {
        method: 'POST',
        headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
        body: body.toString()
      }).then(function (resp) { return resp.json(); });
    }

    // Try server proxy first, fall back to direct PKCE exchange
    var proxyUrl = sameOriginUrl(backendBase(tenant) + '/oauth/token-exchange');
    console.log('[oauth] exchanging code via proxy');

    // Backend proxy URL is constrained to the current origin.
    // foxguard: ignore[js/no-ssrf]
    fetch(proxyUrl, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        provider_id: storedProvider.id,
        token_url: storedProvider.token_url,
        code: code,
        redirect_uri: redirectUri || window.location.href.split('?')[0],
        client_id: storedProvider.client_id,
        code_verifier: codeVerifier || ''
      })
    })
      .then(function (resp) {
        if (!resp.ok) throw new Error('proxy returned ' + resp.status);
        return resp.json();
      })
      .then(handleTokens)
      .catch(function (proxyErr) {
        console.warn('[oauth] proxy failed:', proxyErr.message, '— trying direct PKCE exchange');
        directTokenExchange()
          .then(handleTokens)
          .catch(function (directErr) {
            console.warn('[oauth] direct exchange also failed:', directErr.message);
            saveOAuthSession('authenticated', 'oauth-code');
            removeOAuthOverlay();
            injectLogoutButton();
          });
      });

    return true;
  }

  function cleanCallbackParams(params) {
    params.delete('code');
    params.delete('state');
    params.delete('error');
    params.delete('error_description');
    var cleanUrl = window.location.pathname;
    var remaining = params.toString();
    if (remaining) {
      cleanUrl += '?' + remaining;
    }
    window.history.replaceState({}, '', cleanUrl);
  }

  /**
   * Generate a random string for PKCE code verifier.
   */
  function generateCodeVerifier() {
    var array = new Uint8Array(32);
    crypto.getRandomValues(array);
    return btoa(String.fromCharCode.apply(null, array))
      .replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
  }

  /**
   * Compute S256 code challenge from a code verifier.
   */
  async function generateCodeChallenge(verifier) {
    var data = new TextEncoder().encode(verifier);
    var digest = await crypto.subtle.digest('SHA-256', data);
    return btoa(String.fromCharCode.apply(null, new Uint8Array(digest)))
      .replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
  }

  /**
   * Initiate OAuth flow by building the authorize URL directly (PKCE).
   * No server-side call needed — standard SPA OIDC flow.
   */
  /**
   * Initiate OAuth for a specific provider.
   * Provider object: { id, label, auth_url, token_url, client_id, scopes }
   */
  function initiateOAuthFlow(provider) {
    // Dummy/guest providers skip OAuth — just save session and proceed
    if (provider.type === 'dummy') {
      saveOAuthSession('guest', 'dummy');
      try {
        sessionStorage.setItem(oauthStorageKey('user_name'), 'Guest');
        sessionStorage.setItem(oauthStorageKey('provider'), JSON.stringify({ id: provider.id, type: 'dummy' }));
      } catch (_) {}
      removeOAuthOverlay();
      injectLogoutButton();
      return;
    }

    if (!provider.auth_url || !provider.client_id) {
      showAuthError('OAuth not configured for ' + (provider.label || provider.id) + '. Set auth_url and client_id.');
      return;
    }

    // Use clean URL without query params — Google requires exact redirect_uri match
    var redirectUri = window.location.href.split('?')[0];

    var codeVerifier = generateCodeVerifier();
    try {
      sessionStorage.setItem(oauthStorageKey('code_verifier'), codeVerifier);
      sessionStorage.setItem(oauthStorageKey('redirect_uri'), redirectUri);
      // Store which provider was used so callback knows token_url + client_id
      sessionStorage.setItem(oauthStorageKey('provider'), JSON.stringify(provider));
    } catch (_) {}

    var scopes = provider.scope || provider.scopes || 'openid profile email';
    var state = 'webchat-' + Date.now() + '-' + Math.random().toString(36).slice(2, 8);

    try {
      sessionStorage.setItem(oauthStorageKey('state'), state);
    } catch (_) {}

    generateCodeChallenge(codeVerifier).then(function (codeChallenge) {
      var params = new URLSearchParams({
        response_type: 'code',
        client_id: provider.client_id,
        redirect_uri: redirectUri,
        scope: scopes,
        state: state,
        code_challenge: codeChallenge,
        code_challenge_method: 'S256',
        access_type: 'offline',
        prompt: 'select_account'
      });

      var authorizeUrl = trustedHttpUrl(provider.auth_url) + '?' + params.toString();
      console.log('[oauth] redirecting to provider:', provider.id, provider.auth_url);
      // OAuth authorization endpoints are provider-configured HTTP(S) URLs.
      // foxguard: ignore[js/no-open-redirect]
      window.location.assign(authorizeUrl);
    });
  }

  // Map provider IDs to friendly display names
  var PROVIDER_LABELS = {
    'oauth-oidc-generic': 'SSO',
    'generic_oidc': 'SSO',
    'google': 'Google',
    'microsoft': 'Microsoft',
    'msgraph': 'Microsoft',
    'github': 'GitHub',
    'apple': 'Apple',
    'okta': 'Okta',
    'auth0': 'Auth0',
    'keycloak': 'Keycloak'
  };

  function providerLabel(providerId) {
    return PROVIDER_LABELS[providerId] || providerId;
  }

  function performLogout() {
    clearOAuthSession();
    clearAppAuthSession();
    window.location.reload();
  }

  /**
   * Remove any existing OAuth overlay.
   */
  function removeOAuthOverlay() {
    var existing = document.getElementById('greentic-oauth-overlay');
    if (existing) existing.remove();
  }

  /**
   * Show a fullscreen login overlay with buttons for each OAuth provider.
   */
  function showLoginScreen(authConfig) {
    removeOAuthOverlay();
    var providers = (authConfig && authConfig.providers) || [];
    var overlay = document.createElement('div');
    overlay.id = 'greentic-oauth-overlay';
    overlay.style.cssText = 'position:fixed;inset:0;z-index:99999;display:flex;align-items:center;justify-content:center;background:#f8fafb;font-family:Poppins,system-ui,-apple-system,sans-serif;';
    var card = document.createElement('div');
    card.style.cssText = 'max-width:380px;width:90%;padding:48px 36px;border-radius:20px;box-shadow:0 8px 32px rgba(0,0,0,0.06);text-align:center;background:#fff;border:1px solid #e5e7eb;';
    // Logo icon
    var logoWrap = document.createElement('div');
    logoWrap.style.cssText = 'width:56px;height:56px;border-radius:50%;background:#ecfdf5;display:flex;align-items:center;justify-content:center;margin:0 auto 20px;';
    logoWrap.innerHTML = '<svg width="28" height="28" viewBox="0 0 24 24" fill="none" stroke="#059669" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M20 2H4c-1.1 0-2 .9-2 2v18l4-4h14c1.1 0 2-.9 2-2V4c0-1.1-.9-2-2-2z"/></svg>';
    card.appendChild(logoWrap);
    card.innerHTML += '<h2 style="margin:0 0 6px;font-size:1.375rem;font-weight:600;color:#1f2937;">Welcome</h2>' +
      '<p style="margin:0 0 32px;color:#6b7280;font-size:0.875rem;line-height:1.5;">Sign in to start chatting</p>';
    var btnContainer = document.createElement('div');
    btnContainer.style.cssText = 'display:flex;flex-direction:column;gap:12px;';
    providers.forEach(function (provider) {
      var label = provider.label || providerLabel(provider.id) || 'SSO';
      var btn = document.createElement('button');
      // Avoid double prefix like "Sign in with Sign in with Google"
      btn.textContent = /^(sign in|log in|continue)/i.test(label) ? label : 'Sign in with ' + label;
      btn.style.cssText = 'padding:12px 28px;border:none;border-radius:12px;background:#059669;color:#fff;font-size:15px;font-weight:500;cursor:pointer;transition:background .2s;min-width:200px;';
      btn.onmouseover = function () { btn.style.background = '#047857'; };
      btn.onmouseout = function () { btn.style.background = '#059669'; };
      btn.onclick = function () {
        btn.disabled = true;
        btn.textContent = 'Redirecting...';
        btn.style.opacity = '0.7';
        initiateOAuthFlow(provider);
      };
      btnContainer.appendChild(btn);
    });
    if (providers.length === 0) {
      card.innerHTML += '<p style="color:#ef4444;font-size:13px;">No OAuth providers configured.</p>';
    }
    card.appendChild(btnContainer);
    overlay.appendChild(card);
    document.body.appendChild(overlay);
  }

  function showAuthError(message) {
    removeOAuthOverlay();
    var overlay = document.createElement('div');
    overlay.id = 'greentic-oauth-overlay';
    overlay.style.cssText = 'position:fixed;inset:0;z-index:99999;display:flex;align-items:center;justify-content:center;background:#f8fafb;font-family:Poppins,system-ui,-apple-system,sans-serif;';
    var card = document.createElement('div');
    card.style.cssText = 'max-width:380px;width:90%;padding:48px 36px;border-radius:20px;box-shadow:0 8px 32px rgba(0,0,0,0.06);text-align:center;background:#fff;border:1px solid #e5e7eb;';
    var title = document.createElement('h2');
    title.style.cssText = 'margin:0 0 8px;font-size:22px;font-weight:600;color:#ef4444;';
    title.textContent = 'Something went wrong';
    var detail = document.createElement('p');
    detail.style.cssText = 'margin:0 0 28px;color:#666;font-size:14px;line-height:1.5;';
    detail.textContent = message;
    card.appendChild(title);
    card.appendChild(detail);
    var retryBtn = document.createElement('button');
    retryBtn.textContent = 'Try Again';
    retryBtn.style.cssText = 'padding:12px 28px;border:none;border-radius:12px;background:#059669;color:#fff;font-size:15px;font-weight:500;cursor:pointer;min-width:200px;';
    retryBtn.onclick = function () {
      clearOAuthSession();
      window.location.reload();
    };
    card.appendChild(retryBtn);
    overlay.appendChild(card);
    document.body.appendChild(overlay);
  }

  /**
   * Inject logout button into the existing header bar (next to locale picker).
   * Uses MutationObserver to wait for the header to render.
   */
  // Flag: should inject logout when locale picker mounts
  window.__OAUTH_SHOW_LOGOUT__ = false;
  var logoutObserverStarted = false;
  var logoutObserverTimer = null;

  function injectLogoutButton() {
    window.__OAUTH_SHOW_LOGOUT__ = true;
    startLogoutObserver();
    tryInjectLogout();
  }

  function startLogoutObserver() {
    if (logoutObserverStarted || typeof MutationObserver === 'undefined') return;
    if (!document.body) {
      window.addEventListener('DOMContentLoaded', startLogoutObserver, { once: true });
      return;
    }
    logoutObserverStarted = true;
    new MutationObserver(function () {
      if (!window.__OAUTH_SHOW_LOGOUT__) return;
      if (logoutObserverTimer) return;
      logoutObserverTimer = setTimeout(function () {
        logoutObserverTimer = null;
        tryInjectLogout();
      }, 50);
    }).observe(document.body, { childList: true, subtree: true });
  }

  function tryInjectLogout() {
    if (document.getElementById('greentic-logout-btn')) return;
    if (!hasAnyAuthSession()) return;

    // Prefer greentic-header-controls (built by locale picker), fallback to raw mount point
    var container = document.getElementById('greentic-header-controls')
      || document.getElementById('locale-picker-mount');
    if (container) {
      if (container.id === 'greentic-header-controls') {
        var div = document.createElement('span');
        div.className = 'topbar__divider';
        container.appendChild(div);
      }
      appendLogoutToContainer(container);
      return;
    }
  }

  function appendLogoutToContainer(container) {
    // Prevent duplicate injection
    if (document.getElementById('greentic-logout-btn')) return;
    // Show user avatar + name if available
    var userName, userPicture;
    try {
      userName = sessionStorage.getItem(oauthStorageKey('user_name'));
      userPicture = sessionStorage.getItem(oauthStorageKey('user_picture'));
    } catch (_) {}

    if (userPicture) {
      var avatar = document.createElement('img');
      avatar.src = userPicture;
      avatar.referrerPolicy = 'no-referrer';
      avatar.style.cssText = 'width:24px;height:24px;border-radius:50%;object-fit:cover;';
      container.appendChild(avatar);
    }

    if (userName) {
      var nameEl = document.createElement('span');
      nameEl.textContent = userName;
      nameEl.style.cssText = 'font-size:12px;color:var(--text-muted, #555);max-width:120px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;';
      container.appendChild(nameEl);
    }

    var btn = document.createElement('button');
    btn.textContent = uiT('header.logout', 'Logout');
    btn.id = 'greentic-logout-btn';
    btn.style.cssText = 'padding:4px 12px;border:1px solid #ccc;border-radius:4px;background:#fff;color:#555;font-size:12px;cursor:pointer;transition:background .15s;white-space:nowrap;';
    btn.onmouseover = function () { btn.style.background = '#f0f0f0'; };
    btn.onmouseout = function () { btn.style.background = '#fff'; };
    btn.onclick = performLogout;
    container.appendChild(btn);
  }

  // ---------------------------------------------------------------------------
  // OAuth gate: fetch auth config and gate the SPA if needed
  // ---------------------------------------------------------------------------

  // Cache for auth config (fetched once)
  window.__OAUTH_CONFIG__ = null;
  window.__OAUTH_CHECKED__ = false;

  /**
   * Fetch OAuth config from the backend /auth/config endpoint.
   * Blocks SPA rendering until auth is resolved.
   */
  function checkOAuthGate() {
    var authConfigUrl = sameOriginUrl(backendBase(tenant) + '/auth/config');
    // Backend auth config URL is constrained to the current origin.
    // foxguard: ignore[js/no-ssrf]
    fetch(authConfigUrl)
      .then(function (response) {
        if (!response.ok) {
          console.log('[oauth] auth/config not available, falling back to tenant config');
          return loadAuthFromTenantConfig().then(function (fallback) {
            return fallback || { enabled: false };
          });
        }
        return response.json();
      })
      .then(function (authConfig) {
        if (!authConfig) return;
        return applyAuthConfig(authConfig);
      })
      .catch(function (err) {
        console.warn('[oauth] failed to fetch auth config, trying tenant config:', err);
        return loadAuthFromTenantConfig().then(function (fallback) {
          if (fallback) return applyAuthConfig(fallback);
          window.__OAUTH_CONFIG__ = { enabled: false };
          window.__OAUTH_CHECKED__ = true;
        });
      });
  }

  function shouldUseTenantAuthFallback() {
    return new URLSearchParams(window.location.search).has('loginRequired');
  }

  function loadAuthFromTenantConfig() {
    var basePath = guiBase.replace(/\/$/, '');
    // Try tenant-specific file first, then default.json
    var urls = [
      basePath + '/config/tenants/' + tenant + '.json',
      basePath + '/config/tenants/default.json'
    ];
    return tryFetchFirst(urls)
      .then(function (r) { return r ? r.json() : null; })
      .then(function (data) {
        if (!data || !data.auth) return null;
        var enabledProviders = (data.auth.providers || []).filter(function (p) { return p.enabled; });
        if (enabledProviders.length === 0) return null;
        return {
          enabled: enabledProviders.length > 0,
          providers: data.auth.providers,
          source: 'tenant-config'
        };
      })
      .catch(function () { return null; });
  }

  function applyAuthConfig(authConfig) {
        // Normalize provider field names and filter disabled providers
        if (authConfig.providers) {
          authConfig.providers = authConfig.providers
            .filter(function (p) { return p.enabled !== false; })
            .map(function (p) {
              return {
                id: p.id,
                label: p.label,
                type: p.type,
                enabled: p.enabled,
                auth_url: p.auth_url || p.authorizationUrl,
                token_url: p.token_url || p.tokenUrl,
                client_id: p.client_id || p.clientId,
                redirect_uri: p.redirect_uri || p.redirectUri,
                scope: p.scope || p.scopes,
                response_type: p.response_type || p.responseType || 'code'
              };
            });
        }
        window.__OAUTH_CONFIG__ = authConfig;
        window.__OAUTH_CHECKED__ = true;
        console.log('[oauth] auth config:', authConfig.enabled ? 'enabled' : 'disabled', 'providers:', (authConfig.providers || []).length);

        if (!authConfig.enabled) {
          return; // No auth required, SPA proceeds normally
        }

        if (authConfig.source === 'tenant-config') {
          // The React app renders the skin-aware login page from tenant
          // config. Do not stack the legacy runtime OAuth overlay on top.
          window.__OAUTH_SHOW_LOGOUT__ = true;
          tryInjectLogout();
          return;
        }

        // Step 1: Check if returning from OAuth callback (?code=...&state=...)
        if (handleOAuthCallback()) {
          // Callback detected — token exchange in progress
          return;
        }

        // Step 2: Check if we already have a valid session
        var session = getOAuthSession();
        if (session) {
          console.log('[oauth] existing session found');
          injectLogoutButton();
          return;
        }

        // Step 3: No session, show login screen
        console.log('[oauth] no session, showing login');
        showLoginScreen(authConfig);
  }

  function tryFetchFirst(urls) {
    if (urls.length === 0) return Promise.resolve(null);
    return originalFetch(urls[0]).then(function (r) {
      if (r.ok) return r;
      return tryFetchFirst(urls.slice(1));
    }).catch(function () {
      return tryFetchFirst(urls.slice(1));
    });
  }

  // NOTE: checkOAuthGate is called AFTER the fetch interceptor is installed (see below)

  // ---------------------------------------------------------------------------
  // Fetch interceptor (tenant config + skin.json patching)
  // ---------------------------------------------------------------------------

  // Token cache: holds {token, expires_in, expires_at} keyed in localStorage.
  // We refresh BEFORE the server-side TTL elapses so a request never lands on
  // an expired token. The 60s buffer matches the rate-limit window — if the
  // server ever reduces TTL below the buffer, we just always refetch (which
  // is the same behaviour as having no cache).
  // Local storage cache key, not credential material. Include origin, tenant,
  // env, token URL, and Direct Line domain so bundles sharing 127.0.0.1:8080
  // cannot reuse credentials signed by another bundle's jwt_signing_key.
  // foxguard: ignore[js/no-hardcoded-secret]
  var DIRECT_LINE_CACHE_VERSION = 'v2';
  // foxguard: ignore[js/no-hardcoded-secret]
  var LEGACY_TOKEN_CACHE_KEY = 'greentic_dl_token';
  var LEGACY_CONVERSATION_CACHE_KEY = 'greentic_dl_conversation';
  var TOKEN_REFRESH_BUFFER_MS = 60 * 1000;
  var DIRECT_LINE_CONVERSATION_TTL_MS = 30 * 60 * 1000;

  function stableCachePart(value) {
    return encodeURIComponent(String(value || '').trim().toLowerCase());
  }

  function directLineTokenUrl() {
    var base = backendBase(tenant);
    if (bundleId) base += '/' + encodeURIComponent(bundleId);
    return base + '/token?env=' + encodeURIComponent(env) + '&tenant=' + encodeURIComponent(tenant);
  }

  function directLineDomain() {
    var base = backendBase(tenant);
    if (bundleId) base += '/' + encodeURIComponent(bundleId);
    return base + '/v3/directline';
  }

  function directLineCacheKey(kind) {
    return [
      'greentic',
      DIRECT_LINE_CACHE_VERSION,
      'dl',
      kind,
      stableCachePart(window.location.origin),
      stableCachePart(tenant),
      stableCachePart(env),
      stableCachePart(directLineTokenUrl()),
      stableCachePart(directLineDomain()),
      stableCachePart(flowId)
    ].join(':');
  }

  var TOKEN_CACHE_KEY = directLineCacheKey('token');
  var CONVERSATION_CACHE_KEY = directLineCacheKey('conversation');
  var DIRECT_LINE_AUTH_RETRY_KEY = directLineCacheKey('auth-retry');

  function clearLegacyDirectLineCache() {
    try { localStorage.removeItem(LEGACY_TOKEN_CACHE_KEY); } catch (_) {}
    try { localStorage.removeItem(LEGACY_CONVERSATION_CACHE_KEY); } catch (_) {}
  }

  clearLegacyDirectLineCache();

  function readCachedToken() {
    try {
      var raw = localStorage.getItem(TOKEN_CACHE_KEY);
      if (!raw) return null;
      var parsed = JSON.parse(raw);
      if (!parsed || !parsed.token || !parsed.expires_at) return null;
      if (parsed.expires_at - Date.now() <= TOKEN_REFRESH_BUFFER_MS) return null;
      return parsed;
    } catch (_) {
      return null;
    }
  }

  function writeCachedToken(payload) {
    try {
      var ttlMs = (Number(payload.expires_in) || 0) * 1000;
      if (ttlMs <= TOKEN_REFRESH_BUFFER_MS) return;
      var record = {
        token: payload.token,
        expires_in: payload.expires_in,
        expires_at: Date.now() + ttlMs,
      };
      localStorage.setItem(TOKEN_CACHE_KEY, JSON.stringify(record));
    } catch (_) {}
  }

  function readCachedConversation() {
    try {
      var raw = localStorage.getItem(CONVERSATION_CACHE_KEY);
      if (!raw) return null;
      var conv = JSON.parse(raw);
      if (!conv || !conv.conversationId || !conv.streamUrl || !conv.timestamp) return null;
      if ((Date.now() - conv.timestamp) >= DIRECT_LINE_CONVERSATION_TTL_MS) return null;
      return conv;
    } catch (_) {
      return null;
    }
  }

  function writeCachedConversation(payload) {
    try {
      if (!payload || !payload.conversationId) return;
      payload.timestamp = Date.now();
      localStorage.setItem(CONVERSATION_CACHE_KEY, JSON.stringify(payload));
    } catch (_) {}
  }

  function clearDirectLineCache() {
    try { localStorage.removeItem(TOKEN_CACHE_KEY); } catch (_) {}
    try { localStorage.removeItem(CONVERSATION_CACHE_KEY); } catch (_) {}
    clearLegacyDirectLineCache();
  }

  function resetDirectLineAuthRetry() {
    try { sessionStorage.removeItem(DIRECT_LINE_AUTH_RETRY_KEY); } catch (_) {}
  }

  function reloadOnceAfterDirectLineAuthFailure() {
    clearDirectLineCache();
    try {
      if (sessionStorage.getItem(DIRECT_LINE_AUTH_RETRY_KEY) === '1') return false;
      sessionStorage.setItem(DIRECT_LINE_AUTH_RETRY_KEY, '1');
    } catch (_) {
      if (window.__GREENTIC_DIRECT_LINE_AUTH_RETRY__) return false;
      window.__GREENTIC_DIRECT_LINE_AUTH_RETRY__ = true;
    }
    console.warn('[bootstrap] Direct Line returned 401; cleared cached token/conversation and reloading once');
    window.location.reload();
    return true;
  }

  function injectGuestIdIntoBody(init) {
    var nextInit = Object.assign({}, init || {});
    var nextHeaders = nextInit.headers ? Object.assign({}, nextInit.headers) : {};
    var hasContentType = Object.keys(nextHeaders).some(function (k) {
      return k.toLowerCase() === 'content-type';
    });
    if (!hasContentType) {
      nextHeaders['Content-Type'] = 'application/json';
    }
    var existing = {};
    if (nextInit.body) {
      try { existing = JSON.parse(nextInit.body); } catch (_) { existing = {}; }
    }
    existing.user = existing.user || {};
    if (!existing.user.id) existing.user.id = guestId;
    nextInit.headers = nextHeaders;
    nextInit.body = JSON.stringify(existing);
    nextInit.method = nextInit.method || 'POST';
    return nextInit;
  }

  // ---------------------------------------------------------------------------
  // XHR interceptor — botframework-directlinejs (used by Bot Framework Webchat)
  // dispatches its requests via XMLHttpRequest, not fetch. The picker's locale
  // therefore never reaches the server's POST /v3/directline/conversations
  // through the fetch wrapper below. We patch XHR open/send to inject the
  // X-Greentic-Locale header on the conversation-create call so the autoStart
  // envelope can pick the right language for the welcome card.
  // ---------------------------------------------------------------------------
  if (typeof window.XMLHttpRequest === 'function') {
    var XHRProto = window.XMLHttpRequest.prototype;
    var origOpen = XHRProto.open;
    var origSend = XHRProto.send;
    XHRProto.open = function (method, url) {
      this.__gtcMethod = (method || '').toUpperCase();
      this.__gtcUrl = url;
      return origOpen.apply(this, arguments);
    };
    XHRProto.send = function (body) {
      try {
        this.__gtcBody = body;
        if ((selectedLocale || flowId) && this.__gtcMethod === 'POST') {
          var path = '';
          try { path = new URL(this.__gtcUrl, window.location.href).pathname; } catch (_) {}
          if (/\/v3\/directline\/conversations\/?$/i.test(path)) {
            if (selectedLocale) {
              this.setRequestHeader('X-Greentic-Locale', selectedLocale);
            }
            if (flowId) {
              this.setRequestHeader('X-Greentic-Flow', flowId);
            }
          }
        }
        var xhr = this;
        var requestPath = '';
        try { requestPath = new URL(xhr.__gtcUrl, window.location.href).pathname; } catch (_) {}
        if (/\/v3\/directline\//i.test(requestPath)) {
          xhr.addEventListener('loadend', function () {
            if (xhr.status === 401) reloadOnceAfterDirectLineAuthFailure();
          });
        }
      } catch (_) {
        // Header injection is best-effort; failure must not break the request.
      }
      return origSend.apply(this, arguments);
    };
  }

  var originalFetch = window.fetch.bind(window);
  window.fetch = function (input, init) {
    var requestUrl = typeof input === 'string' ? input : input.url;
    var url = new URL(requestUrl, window.location.href);
    console.log('[bootstrap] fetch:', url.pathname);

    // Intercept the Direct Line /token endpoint so we (a) attach the
    // guest_id body the server uses for per-user rate-limit bucketing, and
    // (b) reuse a still-valid token across reloads instead of minting a
    // fresh one every page load.
    if (/(?:^|\/)(?:token|v3\/directline\/tokens\/generate)$/i.test(url.pathname)) {
      var cached = readCachedToken();
      if (cached) {
        console.log('[bootstrap] reusing cached token (expires in',
          Math.round((cached.expires_at - Date.now()) / 1000), 's)');
        return Promise.resolve(new Response(JSON.stringify({
          token: cached.token,
          expires_in: cached.expires_in,
        }), {
          status: 200,
          headers: { 'Content-Type': 'application/json' },
        }));
      }
      var nextInit = injectGuestIdIntoBody(init);
      return originalFetch(input, nextInit).then(function (response) {
        if (response.status === 429) {
          var retryAfter = response.headers.get('Retry-After');
          console.warn('[bootstrap] /token rate-limited; Retry-After=', retryAfter);
          return response;
        }
        if (!response.ok) return response;
        var cloned = response.clone();
        cloned.json().then(function (data) {
          if (data && data.token && data.expires_in) {
            writeCachedToken(data);
            resetDirectLineAuthRetry();
            console.log('[bootstrap] cached new token, ttl=', data.expires_in, 's');
          }
        }).catch(function () {});
        return response;
      });
    }

    // Intercept Direct Line /conversations POST to persist conversation across page reloads.
    if (/\/v3\/directline\/conversations\/?$/i.test(url.pathname) && init && init.method === 'POST') {
      // Forward the picker locale so the server-side autoStart envelope
      // (which has no activity body) can resolve i18n tokens for the
      // welcome card. POST /activities already carries `locale` in the
      // BotFramework activity body, but conversation creation does not.
      if (selectedLocale) {
        init.headers = init.headers || {};
        if (init.headers instanceof Headers) {
          init.headers.set('X-Greentic-Locale', selectedLocale);
        } else if (Array.isArray(init.headers)) {
          init.headers.push(['X-Greentic-Locale', selectedLocale]);
        } else {
          init.headers['X-Greentic-Locale'] = selectedLocale;
        }
      }
      if (flowId) {
        init.headers = init.headers || {};
        if (init.headers instanceof Headers) {
          init.headers.set('X-Greentic-Flow', flowId);
        } else if (Array.isArray(init.headers)) {
          init.headers.push(['X-Greentic-Flow', flowId]);
        } else {
          init.headers['X-Greentic-Flow'] = flowId;
        }
      }
      var savedConv = readCachedConversation();
      if (savedConv) {
        console.log('[bootstrap] reusing saved conversation:', savedConv.conversationId);
        return Promise.resolve(new Response(JSON.stringify(savedConv), {
          status: 200,
          headers: { 'Content-Type': 'application/json' }
        }));
      }
      return originalFetch(input, init).then(function (response) {
        if (response.status === 401 && reloadOnceAfterDirectLineAuthFailure()) return response;
        var cloned = response.clone();
        cloned.json().then(function (data) {
          if (data.conversationId) {
            writeCachedConversation(data);
            console.log('[bootstrap] saved conversation:', data.conversationId);
          }
        }).catch(function () {});
        return response;
      });
    }

    if (/\/config\/tenants\/[^/]+\.json$/i.test(url.pathname)) {
      return originalFetch(input, init).then(async function (response) {
        var tenantId = decodeURIComponent(url.pathname.split('/').pop().replace(/\.json$/i, ''));
        var locale = selectedLocale || 'en-US';
        var payload = null;
        var fallbackPayload = function () {
          return {
            tenant_id: tenantId,
            legacy_skin: 'default',
            branding: { company_name: tenantId }
          };
        };
        var contentType = response.headers && response.headers.get ? (response.headers.get('Content-Type') || '') : '';
        if (response.ok && /(^|[;\s])application\/json($|[;\s])|\+json($|[;\s])/i.test(contentType)) {
          try {
            // Use actual tenant config file and patch missing fields
            payload = await response.json();
          } catch (err) {
            console.warn('[bootstrap] tenant config JSON parse failed, using route tenant fallback:', tenantId, err);
          }
        } else if (response.ok) {
          console.warn('[bootstrap] tenant config returned non-JSON content, using route tenant fallback:', tenantId, contentType || '<unknown>');
        }
        if (!payload) payload = fallbackPayload();
        // Reconcile the skin fields. The SPA selects the skins/<name>/ folder
        // from `legacy_skin`, but greentic-setup's sync_skin writes the
        // operator's chosen skin into the modern `skin` field only —
        // `legacy_skin` keeps the default.json scaffold value ("default").
        // Without this, a tenant set up with a custom skin (e.g. 3aigent)
        // renders the default skin. `skin` is authoritative when present.
        if (typeof payload.skin === 'string' && payload.skin.trim()) {
          payload.legacy_skin = payload.skin.trim();
        }
        // Ensure directline config is set — bundle-scoped when a bundle is
        // present so the server routes to the correct deployment.
        var dlPrefix = '/v1/messaging/webchat/' + encodeURIComponent(tenantId);
        if (bundleId) dlPrefix += '/' + encodeURIComponent(bundleId);
        payload.webchat = payload.webchat || {};
        payload.webchat.directline = payload.webchat.directline || {};
        payload.webchat.directline.token_url = window.location.origin + dlPrefix + '/token';
        payload.webchat.directline.domain = window.location.origin + dlPrefix + '/v3/directline';
        payload.webchat.locale = locale;
        var textInput = (new URLSearchParams(window.location.search).get('textInput') || '').trim().toLowerCase();
        if (textInput === 'false' || textInput === '0' || textInput === 'off' || textInput === 'no' || textInput === 'disabled') {
          payload.webchat.style_options = payload.webchat.style_options || {};
          payload.webchat.style_options.hideSendBox = true;
        }
        console.log('[bootstrap] tenant config patched:', tenantId, 'auth providers:', (payload.auth && payload.auth.providers || []).length);
        return new Response(JSON.stringify(payload), {
          status: 200,
          headers: { 'Content-Type': 'application/json' }
        });
      });
    }

    if (/skins\/[^/]+\/skin\.json$/i.test(url.pathname)) {
      /**
       * Tenant -> skin indirection.
       *
       * The URL path slug (`urlTenant`) identifies the tenant, but the skin
       * (visual theme) is decoupled: tenants/<urlTenant>.json may declare a
       * `skin` field naming a different folder under `skins/`. This lets
       * multiple tenants share a skin and a tenant switch skins without
       * being renamed. The setup wizard's `skin` answer writes this field
       * at deploy time.
       *
       * If the field is absent, missing, or the tenant config fetch fails,
       * we fall through to the original URL — preserving today's behavior
       * (load `skins/<urlTenant>/skin.json`, with the existing 404 -> default
       * fallback below kicking in if that path doesn't exist either).
       *
       * The legacy `legacy_skin` field is intentionally NOT consulted here:
       * its semantics (fallback skin name when the tenant config file is
       * missing entirely) are unchanged.
       */
      return (async function () {
        var effectiveInput = input;
        var effectiveUrlPath = url.pathname;
        var pathTenantMatch = url.pathname.match(/skins\/([^/]+)\/skin\.json$/i);
        var urlTenantSlug = pathTenantMatch ? decodeURIComponent(pathTenantMatch[1]) : null;
        if (urlTenantSlug) {
          try {
            var basePath = guiBase.replace(/\/$/, '');
            var tenantCfgUrl = basePath + '/config/tenants/' + encodeURIComponent(urlTenantSlug) + '.json';
            var tenantCfgResp = await originalFetch(tenantCfgUrl);
            if (tenantCfgResp && tenantCfgResp.ok) {
              var tenantCfg = await tenantCfgResp.json();
              var skinOverride = tenantCfg && typeof tenantCfg.skin === 'string' ? tenantCfg.skin.trim() : '';
              if (skinOverride && skinOverride !== urlTenantSlug) {
                console.log('[bootstrap] tenant config skin override: ' + urlTenantSlug + ' -> ' + skinOverride);
                effectiveUrlPath = url.pathname.replace(/skins\/[^/]+\//, 'skins/' + encodeURIComponent(skinOverride) + '/');
                effectiveInput = new URL(effectiveUrlPath, url).toString();
              }
            }
          } catch (_) {
            // Tenant config unreachable or unparseable: keep the original
            // skin URL; the existing 404 -> default fallback still applies.
          }
        }
        var response = await originalFetch(effectiveInput, init);
        // Fallback: tenant skin not found or SPA returned HTML
        var skinData;
        if (response.ok) {
          var ct = response.headers.get('content-type') || '';
          if (ct.includes('json')) {
            skinData = await response.json();
          } else {
            response = null;
          }
        }
        if (!skinData) {
          var fbUrl = effectiveUrlPath.replace(/skins\/[^/]+\//, 'skins/default/');
          console.log('[bootstrap] skin not found, falling back to default:', fbUrl);
          var fbResp = await originalFetch(fbUrl);
          if (!fbResp.ok) return fbResp;
          skinData = await fbResp.json();
        }
        skinData.directLine = skinData.directLine || {};
        var ctxParams = 'env=' + encodeURIComponent(env) + '&tenant=' + encodeURIComponent(tenant);
        var skinDlPrefix = '/v1/messaging/webchat/' + encodeURIComponent(tenant);
        if (bundleId) skinDlPrefix += '/' + encodeURIComponent(bundleId);
        skinData.directLine.tokenUrl = window.location.origin + skinDlPrefix + '/token?' + ctxParams;
        skinData.directLine.domain = window.location.origin + skinDlPrefix + '/v3/directline';
        if (selectedLocale) {
          skinData.webchat = skinData.webchat || {};
          skinData.webchat.locale = selectedLocale;
        }
        // Theme-aware Web Chat assets. Skin opts in via
        // `webchat.styleOptionsThemed: true`; runtime rewrites the
        // `styleOptions.json` AND `hostconfig.json` URLs to
        // `<name>-<theme>.json` based on the SPA's persisted theme. Read
        // order matches the locale picker's theme button:
        // sessionStorage["greentic-theme"], then <html data-theme>, then
        // default to dark. For first-load when neither is set, also pin
        // <html data-theme> to the resolved value so SPA's
        // applyDarkModeInlineOverrides and our CSS pick up the matching
        // palette without flicker. Skins that don't set the flag are
        // unaffected.
        if (skinData.webchat && skinData.webchat.styleOptionsThemed === true) {
          // Base default theme: a skin may pin one via webchat.defaultTheme
          // ('light' | 'dark'); otherwise follow the viewer's OS preference.
          // (Previously this hard-defaulted to 'dark', forcing every themed
          // skin dark on first load regardless of the viewer's setting.)
          var dt = skinData.webchat.defaultTheme;
          var theme = (dt === 'light' || dt === 'dark')
            ? dt
            : ((window.matchMedia && window.matchMedia('(prefers-color-scheme: dark)').matches) ? 'dark' : 'light');
          try {
            var saved = sessionStorage.getItem('greentic-theme');
            if (saved === 'light' || saved === 'dark') {
              theme = saved;
            } else {
              var attr = document.documentElement.getAttribute('data-theme');
              if (attr === 'light') theme = 'light';
            }
          } catch (_) { /* keep default */ }
          if (!document.documentElement.getAttribute('data-theme')) {
            document.documentElement.setAttribute('data-theme', theme);
          }
          var pat = /\.json$/i;
          if (skinData.webchat.styleOptions && /styleOptions\.json$/i.test(skinData.webchat.styleOptions)) {
            skinData.webchat.styleOptions = skinData.webchat.styleOptions.replace(
              /styleOptions\.json$/i,
              'styleOptions-' + theme + '.json'
            );
          }
          if (skinData.webchat.adaptiveCardsHostConfig && /hostconfig\.json$/i.test(skinData.webchat.adaptiveCardsHostConfig)) {
            skinData.webchat.adaptiveCardsHostConfig = skinData.webchat.adaptiveCardsHostConfig.replace(
              /hostconfig\.json$/i,
              'hostconfig-' + theme + '.json'
            );
          }
          console.log('[bootstrap] themed Web Chat assets selected:', theme);
        }
        skinData.statusBar = skinData.statusBar || {};
        skinData.statusBar.show = false;
        window.__SKIN__ = skinData;
        // Update topbar title with brand name from skin
        var titleEl = document.querySelector('.topbar__title');
        if (titleEl && skinData.brand && skinData.brand.name) {
          titleEl.textContent = skinData.brand.name;
        }
        console.log('[bootstrap] skin.json patched:', skinData.directLine, 'locale:', skinData.webchat?.locale);
        return new Response(JSON.stringify(skinData), {
          status: 200,
          headers: { 'Content-Type': 'application/json' }
        });
      })();
    }

    return originalFetch(input, init);
  };

  // Run OAuth check AFTER fetch interceptor is installed
  checkOAuthGate();

  // ---------------------------------------------------------------------------
  // Locale picker
  // ---------------------------------------------------------------------------

  // Fetch the flow pack's i18n manifest to determine which locales have
  // actual translations.  Only those locales appear in the picker.
  function fetchAvailableFlowLocales(callback) {
    // Skin-level override: if skin.json declares
    // `webchat.localePickerLocales: [...]`, honor it directly and skip the
    // flow-card manifest probe. Useful when the demo flow ships only
    // English cards but the operator still wants the GUI's locale picker
    // to expose the wider set of UI translations under `i18n/<code>.json`.
    if (window.__SKIN__ &&
        window.__SKIN__.webchat &&
        Array.isArray(window.__SKIN__.webchat.localePickerLocales) &&
        window.__SKIN__.webchat.localePickerLocales.length > 0) {
      var override = {};
      window.__SKIN__.webchat.localePickerLocales.forEach(function (code) {
        if (SUPPORTED_LOCALES[code]) override[code] = SUPPORTED_LOCALES[code];
      });
      if (!override['en']) override['en'] = 'English';
      console.log('[bootstrap] locale picker override:', Object.keys(override).length, 'locales');
      callback(override);
      return;
    }
    // The i18n manifest lists locale codes that have card translations.
    // Served from the webchat-gui pack's i18n directory.
    var manifestUrl = guiBase + 'i18n/_manifest.json';
    // GUI i18n manifest is loaded from the packaged GUI base path.
    // foxguard: ignore[js/no-ssrf]
    fetch(manifestUrl)
      .then(function (res) { return res.ok ? res.json() : null; })
      .catch(function () { return null; })
      .then(function (codes) {
        if (Array.isArray(codes) && codes.length > 0) {
          var filtered = {};
          codes.forEach(function (code) {
            if (SUPPORTED_LOCALES[code]) {
              filtered[code] = SUPPORTED_LOCALES[code];
            }
          });
          if (!filtered['en']) filtered['en'] = 'English';
          callback(filtered);
        } else {
          // Manifest not available → show only English (no card translations found)
          callback({ en: 'English' });
        }
      });
  }

  // Build locale picker + logout button inside a single flex container
  function initLocalePicker(mountEl) {
    function buildPicker(locales) {
      // Create flex container that holds both locale picker and logout
      var container = document.createElement('div');
      container.id = 'greentic-header-controls';
      container.style.cssText = 'display:flex;align-items:center;gap:8px;';

      // Locale picker
      var globeSvg = '<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><line x1="2" y1="12" x2="22" y2="12"/><path d="M12 2a15.3 15.3 0 0 1 4 10 15.3 15.3 0 0 1-4 10 15.3 15.3 0 0 1-4-10A15.3 15.3 0 0 1 12 2z"/></svg>';

      var wrapper = document.createElement('label');
      wrapper.className = 'locale-picker';
      // Static local SVG markup; no user-controlled input reaches this assignment.
      // foxguard: ignore[js/no-xss-innerhtml]
      wrapper.innerHTML = globeSvg;

      var selectEl = document.createElement('select');
      selectEl.className = 'locale-picker__select';

      var codes = Object.keys(locales).sort(function (a, b) {
        return locales[a].localeCompare(locales[b]);
      });
      var current = selectedLocale || 'en';
      codes.forEach(function (code) {
        var opt = document.createElement('option');
        opt.value = code;
        opt.textContent = locales[code] + ' (' + code + ')';
        if (code === current) opt.selected = true;
        selectEl.appendChild(opt);
      });

      selectEl.addEventListener('change', function () {
        var params = new URLSearchParams(window.location.search);
        params.set('lang', this.value);
        window.location.search = params.toString();
      });

      wrapper.appendChild(selectEl);

      // Theme toggle button
      var themeBtn = document.createElement('button');
      themeBtn.className = 'theme-toggle';
      themeBtn.title = 'Toggle theme';
      var isDark = document.documentElement.getAttribute('data-theme') === 'dark' ||
        (!document.documentElement.getAttribute('data-theme') && window.matchMedia('(prefers-color-scheme: dark)').matches);
      themeBtn.textContent = isDark ? '☀️' : '🌙';
      // Override WebChat SDK inline styles that use !important (can't beat with CSS alone)
      function applyDarkModeInlineOverrides(dark) {
        // Per-theme palette. Web Chat 4.x loads styleOptions once at mount,
        // so realtime theme switches need both dark→light AND light→dark
        // to PROACTIVELY set the new palette (clearing inline styles isn't
        // enough — Web Chat's React state still holds the mount-time
        // styleOptions and re-applies them on every DOM mutation).
        var palette = dark ? {
          inputColor: '#e5e7eb',
          inputBg: '#111827',
          sendBoxBg: '#111827',
          sendBoxBorder: '#374151',
          bubbleBg: '#1f2937',
          bubbleColor: '#e5e7eb',
          transcriptBg: '#111827',
          userBubbleBg: '#0e7490',
          userBubbleColor: '#ffffff'
        } : {
          inputColor: '#1f2937',
          inputBg: '#ffffff',
          sendBoxBg: '#ffffff',
          sendBoxBorder: '#e2e8f0',
          bubbleBg: '#ffffff',
          bubbleColor: '#0f172a',
          transcriptBg: '#f8fafc',
          userBubbleBg: '#0891b2',
          userBubbleColor: '#ffffff'
        };

        // Send box input
        var inputs = document.querySelectorAll('.webchat__send-box-text-box__input');
        for (var i = 0; i < inputs.length; i++) {
          inputs[i].style.setProperty('color', palette.inputColor, 'important');
          inputs[i].style.setProperty('background', palette.inputBg, 'important');
        }
        // Send box container + descendants — strip inline whites/darks Web
        // Chat baked in at mount, then set explicit theme-correct values on
        // the wrapper.
        var sendBoxes = document.querySelectorAll('.webchat__send-box, .webchat__send-box *');
        for (var j = 0; j < sendBoxes.length; j++) {
          var el = sendBoxes[j];
          if (el.tagName === 'BUTTON' || el.tagName === 'SVG' || el.tagName === 'PATH') continue;
          el.style.setProperty('background-color', 'transparent', 'important');
          el.style.setProperty('background', 'transparent', 'important');
        }
        // Send box wrapper itself
        var sendBoxRoot = document.querySelectorAll('.webchat__send-box');
        for (var k = 0; k < sendBoxRoot.length; k++) {
          sendBoxRoot[k].style.setProperty('background', palette.sendBoxBg, 'important');
          sendBoxRoot[k].style.setProperty('border-top-color', palette.sendBoxBorder, 'important');
        }
        // Bot bubble (Adaptive Card containers)
        var botBubbles = document.querySelectorAll('.webchat__bubble:not(.webchat__bubble--from-user) .webchat__bubble__content');
        for (var b = 0; b < botBubbles.length; b++) {
          botBubbles[b].style.setProperty('background', palette.bubbleBg, 'important');
          botBubbles[b].style.setProperty('color', palette.bubbleColor, 'important');
          // Strip every nested div's inline bg so the bubble bg shows
          // through cleanly in both directions (dark→light and light→dark).
          var children = botBubbles[b].querySelectorAll('div[style]');
          for (var c = 0; c < children.length; c++) {
            children[c].style.setProperty('background-color', 'transparent', 'important');
          }
        }
        // User bubble — keep brand-cyan filling, just sync the text color.
        var userBubbles = document.querySelectorAll('.webchat__bubble--from-user .webchat__bubble__content');
        for (var u = 0; u < userBubbles.length; u++) {
          userBubbles[u].style.setProperty('background', palette.userBubbleBg, 'important');
          userBubbles[u].style.setProperty('color', palette.userBubbleColor, 'important');
        }
        // Transcript background
        var transcripts = document.querySelectorAll('.webchat__basic-transcript');
        for (var t = 0; t < transcripts.length; t++) {
          transcripts[t].style.setProperty('background-color', palette.transcriptBg, 'important');
        }
      }

      // Watch for WebChat SDK injecting elements with inline styles
      if (typeof MutationObserver !== 'undefined') {
        var darkObserverTimer = null;
        new MutationObserver(function () {
          // Debounce to avoid thrashing
          if (darkObserverTimer) return;
          darkObserverTimer = setTimeout(function () {
            darkObserverTimer = null;
            var themeDark = document.documentElement.getAttribute('data-theme') === 'dark' ||
              (!document.documentElement.getAttribute('data-theme') && window.matchMedia('(prefers-color-scheme: dark)').matches);
            applyDarkModeInlineOverrides(themeDark);
          }, 50);
        }).observe(document.body, { childList: true, subtree: true });
      }

      themeBtn.onclick = function () {
        var html = document.documentElement;
        var current = html.getAttribute('data-theme');
        var next = current === 'dark' ? 'light' : (current === 'light' ? 'dark' : (isDark ? 'light' : 'dark'));
        html.setAttribute('data-theme', next);
        themeBtn.textContent = next === 'dark' ? '☀️' : '🌙';
        try { sessionStorage.setItem('greentic-theme', next); } catch (_) {}
        // Realtime theme switch: keep the chat session alive. Inline
        // overrides cover the surfaces Web Chat sets via styleOptions
        // (send box, bubbles, transcript bg). Anything else picks up the
        // new palette through `[data-theme]` CSS variables. If a few
        // styleOptions-only properties stay stale until next mount,
        // accept the cosmetic delta — losing the conversation on every
        // toggle was the worse trade.
        applyDarkModeInlineOverrides(next === 'dark');
      };
      // Restore saved theme
      try {
        var saved = sessionStorage.getItem('greentic-theme');
        if (saved) {
          document.documentElement.setAttribute('data-theme', saved);
          themeBtn.textContent = saved === 'dark' ? '☀️' : '🌙';
          applyDarkModeInlineOverrides(saved === 'dark');
        }
      } catch (_) {}

      // Build controls with dividers: [theme] | [locale] | [session]
      container.appendChild(themeBtn);

      var div1 = document.createElement('span');
      div1.className = 'topbar__divider';
      container.appendChild(div1);

      container.appendChild(wrapper);

      mountEl.appendChild(container);

      // Now that greentic-header-controls exists, retry logout injection
      if (window.__OAUTH_SHOW_LOGOUT__) {
        tryInjectLogout();
      }
      console.log('[runtime-bootstrap] locale picker initialized, current:', current);
    }

    fetchAvailableFlowLocales(function (locales) {
      buildPicker(locales);
    });
  }

  // Use MutationObserver to detect when #locale-picker-mount appears in the DOM
  var pickerInitialized = false;
  var observer = new MutationObserver(function () {
    if (pickerInitialized) return;
    var mountEl = document.getElementById('locale-picker-mount');
    if (mountEl) {
      pickerInitialized = true;
      observer.disconnect();
      initLocalePicker(mountEl);
    }
  });
  observer.observe(document.documentElement, { childList: true, subtree: true });

  // ---------------------------------------------------------------------------
  // Topbar tenant nav. Reads `nav_links: [...]` from the tenant config JSON
  // and renders one anchor per entry into `#topbar-nav`.
  //
  // Each entry shape (all fields except `label`/`url` optional):
  //   {
  //     "label":    string | { en, id, fr, ... },   // multilingual object
  //     "url":      string,
  //     "external": bool,                            // open in new tab
  //     "num":      string | { en, ... },            // small chip prefix (e.g. "M5")
  //     "tooltip":  {
  //       "eyebrow": string | { en, ... },
  //       "title":   string | { en, ... },
  //       "lede":    string | { en, ... }            // supports inline markup
  //     }
  //   }
  //
  // Operator-set values come from `tenants/<tenant>.json` written by
  // `greentic-setup`'s `sync_nav_links_to_tenant_config`. Locale-keyed
  // labels resolve via selectedLocale → base language → "en" → first
  // non-empty value.
  // ---------------------------------------------------------------------------
  function pickNavLabel(raw) {
    if (typeof raw === 'string') {
      var t = raw.trim();
      return t.length > 0 ? t : null;
    }
    if (!raw || typeof raw !== 'object') return null;
    var locale = selectedLocale || 'en';
    var base = locale.split('-')[0];
    var candidates = [locale, base, 'en'];
    for (var i = 0; i < candidates.length; i++) {
      var v = raw[candidates[i]];
      if (typeof v === 'string') {
        var s = v.trim();
        if (s.length > 0) return s;
      }
    }
    var keys = Object.keys(raw);
    for (var j = 0; j < keys.length; j++) {
      var v2 = raw[keys[j]];
      if (typeof v2 === 'string') {
        var s2 = v2.trim();
        if (s2.length > 0) return s2;
      }
    }
    return null;
  }

  function renderTopbarNav(mountEl, links) {
    while (mountEl.firstChild) mountEl.removeChild(mountEl.firstChild);
    if (!Array.isArray(links) || links.length === 0) return;
    links.forEach(function (entry) {
      if (!entry || typeof entry.url !== 'string') return;
      var url = entry.url.trim();
      if (!url) return;
      var label = pickNavLabel(entry.label);
      if (!label) return;
      var anchor = document.createElement('a');
      anchor.className = 'topbar-nav__link';
      anchor.href = url;
      if (entry.external === true) {
        anchor.target = '_blank';
        anchor.rel = 'noopener noreferrer';
      }
      var num = pickNavLabel(entry.num);
      if (num) {
        var numEl = document.createElement('span');
        numEl.className = 'topbar-nav__num';
        numEl.textContent = num;
        anchor.appendChild(numEl);
      }
      var labelEl = document.createElement('span');
      labelEl.className = 'topbar-nav__label';
      labelEl.textContent = label;
      anchor.appendChild(labelEl);
      if (entry.tooltip && typeof entry.tooltip === 'object') {
        var tip = document.createElement('div');
        tip.className = 'topbar-nav__tooltip';
        var hasContent = false;
        var eyebrow = pickNavLabel(entry.tooltip.eyebrow);
        if (eyebrow) {
          var ebEl = document.createElement('span');
          ebEl.className = 'topbar-nav__tooltip-eyebrow';
          ebEl.textContent = eyebrow;
          tip.appendChild(ebEl);
          hasContent = true;
        }
        var title = pickNavLabel(entry.tooltip.title);
        if (title) {
          var tEl = document.createElement('h3');
          tEl.className = 'topbar-nav__tooltip-title';
          tEl.textContent = title;
          tip.appendChild(tEl);
          hasContent = true;
        }
        var lede = pickNavLabel(entry.tooltip.lede);
        if (lede) {
          var lEl = document.createElement('p');
          lEl.className = 'topbar-nav__tooltip-lede';
          lEl.textContent = lede;
          tip.appendChild(lEl);
          hasContent = true;
        }
        if (hasContent) {
          anchor.classList.add('topbar-nav__link--has-tooltip');
          tip.addEventListener('click', function (ev) {
            ev.preventDefault();
            ev.stopPropagation();
          });
          anchor.appendChild(tip);
        }
      }
      mountEl.appendChild(anchor);
    });
  }

  function fetchTenantNavLinks() {
    // Sequential fallback: try the tenant-specific config first; only fall
    // back to `default.json` when that file is missing OR has no
    // `nav_links` field. `Promise.race` here was a bug — it returned
    // whichever endpoint responded first, so a fast (but empty)
    // `default.json` could mask a slower tenant config that had data.
    var basePath = guiBase.replace(/\/$/, '');
    var primary = basePath + '/config/tenants/' + encodeURIComponent(tenant) + '.json';
    var fallback = basePath + '/config/tenants/default.json';
    function load(url) {
      // Tenant navigation files are resolved under the current page path.
      // foxguard: ignore[js/no-ssrf]
      return fetch(url)
        .then(function (r) { return r && r.ok ? r.json() : null; })
        .catch(function () { return null; });
    }
    return load(primary).then(function (data) {
      if (data && Array.isArray(data.nav_links)) return data.nav_links;
      return load(fallback).then(function (d) {
        return d && Array.isArray(d.nav_links) ? d.nav_links : [];
      });
    });
  }

  var navInitialized = false;
  var navObserver = new MutationObserver(function () {
    if (navInitialized) return;
    var navEl = document.getElementById('topbar-nav');
    if (navEl) {
      navInitialized = true;
      navObserver.disconnect();
      fetchTenantNavLinks().then(function (links) {
        renderTopbarNav(navEl, links);
      });
    }
  });
  navObserver.observe(document.documentElement, { childList: true, subtree: true });
})();
