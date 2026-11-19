// Before every test file (vite.config.ts): the window speaks Traditional
// Chinese, the language the tests' expected words are written in. The
// fallback, English, is what it speaks before `main.ts` picks a language.

import { setLanguage } from "./i18n";

setLanguage("zh-TW");
