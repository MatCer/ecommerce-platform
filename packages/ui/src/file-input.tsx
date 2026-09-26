/**
 * Native `<input type="file">` styled as a Pajamas default button plus the chosen file name.
 * Label it with `FormGroup` (the native control keeps its accessible name and file dialog).
 */
export const fileInputClass =
  "block w-full max-w-full cursor-pointer text-sm text-muted-foreground " +
  "file:mr-3 file:h-control file:cursor-pointer file:rounded-md file:border file:border-border-strong " +
  "file:bg-card file:px-3 file:text-sm file:text-foreground hover:file:bg-subtle " +
  "disabled:cursor-not-allowed disabled:file:cursor-not-allowed disabled:file:bg-subtle disabled:file:text-faint-foreground";
