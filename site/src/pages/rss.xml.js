import rss from '@astrojs/rss';
import { posts } from '../blog';
import { asset } from '../consts';
import { BLOG_DESCRIPTION, BLOG_TITLE, postRoute } from '../../config/blog.mjs';

export async function GET(context) {
  return rss({
    title: BLOG_TITLE,
    description: BLOG_DESCRIPTION,
    site: context.site,
    items: (await posts()).map((post) => ({
      title: post.data.title,
      description: post.data.description,
      pubDate: post.data.pubDate,
      link: asset(postRoute(post.id)),
    })),
  });
}
