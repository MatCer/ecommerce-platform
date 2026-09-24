import nodemailer from "nodemailer";

export interface Mail {
  to: string;
  subject: string;
  text: string;
}

/** Sends one message; the only thing the auth flows need from email. */
export type Mailer = (mail: Mail) => Promise<void>;

/** SMTP mailer (Mailpit locally, SES SMTP in prod). */
export function smtpMailer(smtpUrl: string, from: string): Mailer {
  const transport = nodemailer.createTransport(smtpUrl);
  return async (mail) => {
    await transport.sendMail({ from, ...mail });
  };
}

export const templates = {
  magicLink: (url: string): Omit<Mail, "to"> => ({
    subject: "Sign in to Commerce Platform",
    text: `Use this link to sign in. It expires in 15 minutes and works once.\n\n${url}\n\nIf you did not ask for it, ignore this email.`,
  }),
  verifyEmail: (url: string): Omit<Mail, "to"> => ({
    subject: "Verify your email address",
    text: `Confirm your email address for Commerce Platform:\n\n${url}`,
  }),
  resetPassword: (url: string): Omit<Mail, "to"> => ({
    subject: "Set your Commerce Platform password",
    text: `Use this link to set a new password:\n\n${url}\n\nIf you did not ask for it, ignore this email.`,
  }),
};
