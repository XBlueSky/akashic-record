import { addMessages, init, getLocaleFromNavigator, locale } from 'svelte-i18n';

import en from './en.json';
import zhTW from './zh-TW.json';

addMessages('en', en);
addMessages('zh-TW', zhTW);

const STORAGE_KEY = 'akashic-locale';

function getInitialLocale(): string {
  const stored = localStorage.getItem(STORAGE_KEY);
  if (stored && (stored === 'en' || stored === 'zh-TW')) {
    return stored;
  }

  const nav = getLocaleFromNavigator() || 'en';
  if (nav.startsWith('zh-TW') || nav.startsWith('zh-Hant')) return 'zh-TW';
  if (nav.startsWith('zh')) return 'zh-TW';
  return 'en';
}

init({
  fallbackLocale: 'en',
  initialLocale: getInitialLocale(),
});

locale.subscribe((value) => {
  if (value) {
    localStorage.setItem(STORAGE_KEY, value);
    document.documentElement.lang = value;
  }
});

export { locale };
