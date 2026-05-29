// Waitlist forms. Both forms POST to the same endpoint.
//
// TO GO LIVE: replace YOUR_FORM_ID in index.html (both <form action=...>) with a real
// endpoint. Easiest no-backend options:
//   - Formspree:  https://formspree.io   -> action="https://formspree.io/f/abc123"
//   - Buttondown: https://buttondown.email (newsletter)  - swap the fetch URL below
//   - Tally / ConvertKit / Resend Audiences also work.
// Until then, submissions are logged to the console and show the success state locally.

function wire(formId, noteId) {
  const form = document.getElementById(formId);
  const note = document.getElementById(noteId);
  if (!form) return;

  form.addEventListener("submit", async (e) => {
    e.preventDefault();
    const data = new FormData(form);
    const email = data.get("email");
    const endpoint = form.getAttribute("action");
    const live = endpoint && !endpoint.includes("YOUR_FORM_ID");

    try {
      if (live) {
        const res = await fetch(endpoint, {
          method: "POST",
          body: data,
          headers: { Accept: "application/json" },
        });
        if (!res.ok) throw new Error("bad response");
      } else {
        // Not wired to a provider yet — don't lose the signup, surface it.
        console.info("[waitlist] would submit:", email);
      }
      form.classList.add("done");
      note.textContent = "You're on the list — watch your inbox. 🌱";
      note.classList.add("success");
    } catch {
      note.textContent = "Something went wrong — email hello@freshet.app and we'll add you.";
      note.classList.add("success");
    }
  });
}

wire("hero-form", "hero-note");
wire("cta-form", "cta-note");
