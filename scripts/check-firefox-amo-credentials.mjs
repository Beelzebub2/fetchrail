import { verifyCredentials } from "./firefox-add-ons-policy.mjs";

await verifyCredentials({
  issuer: process.env.AMO_JWT_ISSUER,
  secret: process.env.AMO_JWT_SECRET,
});
