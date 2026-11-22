/**
 * Branding lives in one place so the product can be renamed easily.
 * To rename the app: change the values below, and update `name` / `productName`
 * and `build.productName` / `build.appId` in package.json.
 */
export const BRAND = {
  name: 'POLYMIRA',
  tagline: 'Every console. One universe.',
  /** Used for internal identifiers. Keep it filesystem-safe. */
  slug: 'polymira',
  /** GitHub repository ("owner/name") used for updates and the community store. Empty = disabled. */
  repo: 'hasanlior/polymira' as string,
  /** Data folder names of earlier product names, migrated on first start. */
  legacyNames: ['XPORT'] as string[]
}
