import docs from './src/docs.json' with { type: 'json' };

// `?theme=day`: without it local time picks the palette, and half of CI's runs would audit each.
const QUERY = '?theme=day';
const MEDIAN = { aggregationMethod: 'median' };
// A binary audit that failed one run still has a passing median.
const EVERY_RUN = { aggregationMethod: 'pessimistic' };

export default {
  ci: {
    collect: {
      staticDistDir: './dist',
      numberOfRuns: 5,
      url: ['', ...docs.map((d) => d.route)].map((route) => `http://localhost/${route}${QUERY}`),
    },
    assert: {
      assertions: {
        'categories:accessibility': ['error', { minScore: 0.9, ...MEDIAN }],
        'color-contrast': ['error', { minScore: 1, ...EVERY_RUN }],
        'categories:seo': ['error', { minScore: 0.9, ...MEDIAN }],
        'categories:best-practices': ['error', { minScore: 0.9, ...MEDIAN }],
        'categories:performance': ['error', { minScore: 0.7, ...MEDIAN }],
        'first-contentful-paint': ['error', { maxNumericValue: 3000, ...MEDIAN }],
        'largest-contentful-paint': ['error', { maxNumericValue: 3100, ...MEDIAN }],
        'speed-index': ['error', { maxNumericValue: 4300, ...MEDIAN }],
        interactive: ['error', { maxNumericValue: 3100, ...MEDIAN }],
        'total-blocking-time': ['error', { maxNumericValue: 200, ...MEDIAN }],
        'cumulative-layout-shift': ['error', { maxNumericValue: 0.1, ...MEDIAN }],
        'user-timings:pixtuoid-revealed': ['error', { maxNumericValue: 6500, ...EVERY_RUN }],
      },
    },
    upload: { target: 'filesystem', outputDir: './.lighthouseci' },
  },
};
