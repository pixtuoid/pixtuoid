import { BLOG_ROUTE, markdownPages } from './config/blog.mjs';

// `?theme=day`: without it local time picks the palette, and half of CI's runs would audit each.
const QUERY = '?theme=day';
const MEDIAN = { aggregationMethod: 'median' };
// What must never regress is held to its worst run: a median passes a check that failed one run.
const EVERY_RUN = { aggregationMethod: 'pessimistic' };

export default {
  ci: {
    collect: {
      staticDistDir: './dist',
      numberOfRuns: 5,
      url: ['', BLOG_ROUTE, ...markdownPages().map(({ route }) => route)].map(
        (route) => `http://localhost/${route}${QUERY}`
      ),
    },
    assert: {
      assertions: {
        'categories:accessibility': ['error', { minScore: 0.9, ...MEDIAN }],
        // Its own assertion: it weighs little in the category (lighthouse's core/config/default-config.js), whose
        // budget a total contrast failure still clears.
        'color-contrast': ['error', { minScore: 1, ...EVERY_RUN }],
        'categories:seo': ['error', { minScore: 0.9, ...MEDIAN }],
        'categories:best-practices': ['error', { minScore: 0.9, ...MEDIAN }],
        'categories:performance': ['error', { minScore: 0.7, ...MEDIAN }],
        'first-contentful-paint': ['error', { maxNumericValue: 3000, ...MEDIAN }],
        // LCP, speed-index and interactive are CI's median plus its run spread.
        'largest-contentful-paint': ['error', { maxNumericValue: 3100, ...MEDIAN }],
        'speed-index': ['error', { maxNumericValue: 4300, ...MEDIAN }],
        interactive: ['error', { maxNumericValue: 3100, ...MEDIAN }],
        'total-blocking-time': ['error', { maxNumericValue: 200, ...MEDIAN }],
        'cumulative-layout-shift': ['error', { maxNumericValue: 0.1, ...MEDIAN }],
        'user-timings:pixtuoid-revealed': ['error', { maxNumericValue: 6500, ...EVERY_RUN }],
      },
    },
  },
};
