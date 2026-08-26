// ASP.NET Core's minimal API, answering what `bench/service` answers.
var builder = WebApplication.CreateBuilder(args);
builder.Logging.ClearProviders();
builder.WebHost.UseUrls($"http://127.0.0.1:{args[0]}");

var app = builder.Build();
app.MapGet("/health", () => Results.Content("{\"status\":\"ok\"}", "application/json"));
app.Run();
